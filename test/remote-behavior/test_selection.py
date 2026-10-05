"""按改动挑场景的判据，夹具是临时 git 仓库加一份样例地图。"""

import json
import subprocess

import pytest

from mutate import FAULTS
from selection import ALWAYS, select

PICK = "test_remote.py::test_core_pick[list]"
DOWNLOAD = "test_download.py::test_interrupted_download_discards_partial_file"
SHARED = "crates/ui/src/runtime.rs"


def git(repo, *args):
    return subprocess.run(
        ["git", *args], cwd=repo, check=True, capture_output=True, text=True
    ).stdout.strip()


def commit(repo, files, message):
    for name, text in files.items():
        path = repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", message)
    return git(repo, "rev-parse", "HEAD")


@pytest.fixture
def repo(tmp_path):
    git(tmp_path, "init", "-q", "-b", "master")
    git(tmp_path, "config", "user.email", "t@example.invalid")
    git(tmp_path, "config", "user.name", "t")
    sources = {
        "crates/ui/src/sync/group.rs": "fn a() {}\n",
        "crates/ui/src/download.rs": "fn b() {}\n",
        SHARED: "fn c() {}\n",
        "crates/ui/src/unused.rs": "fn d() {}\n",
        "Cargo.lock": "lock\n",
        "test/remote-behavior/resources.py": "x = 1\n",
        "test/remote-behavior/test_download.py": "x = 1\n",
        "docs/note.md": "doc\n",
    }
    for path, _ in FAULTS.values():
        sources[path] = "fn fault() {}\n"
    for always in ALWAYS:
        sources["test/remote-behavior/" + always.split("::")[0]] = "x = 1\n"
    commit(tmp_path, sources, "base")
    return tmp_path


# 地图记录生成提交、二进制里的全部源文件和每个场景实际执行的文件。
def write_map(repo, commit_id):
    universe = {p.relative_to(repo).as_posix() for p in repo.glob("*/*/src/**/*.rs")}
    universe |= {path for path, _ in FAULTS.values()}
    data = {
        "commit": commit_id,
        "universe": sorted(universe),
        "scenarios": {
            PICK: ["crates/ui/src/sync/group.rs", SHARED],
            DOWNLOAD: ["crates/ui/src/download.rs", SHARED],
        },
    }
    path = repo.parent / "coverage-map.json"
    path.write_text(json.dumps(data))
    return path


def run(repo, changes, map_path=None):
    base = git(repo, "rev-parse", "HEAD")
    if map_path is None:
        map_path = write_map(repo, base)
    head = commit(repo, changes, "change")
    return select(repo, base, head, map_path)


def test_only_covering_scenario_is_selected(repo):
    result = run(repo, {"crates/ui/src/download.rs": "fn b2() {}\n"})
    assert result["mode"] == "subset"
    assert set(result["tests"]) == {DOWNLOAD, *ALWAYS}
    assert any(r["file"] == "crates/ui/src/download.rs" for r in result["reasons"])


def test_shared_file_selects_every_covering_scenario(repo):
    result = run(repo, {SHARED: "fn c2() {}\n"})
    assert set(result["tests"]) == {PICK, DOWNLOAD, *ALWAYS}


def test_documentation_selects_only_cheap_checks(repo):
    result = run(repo, {"docs/note.md": "doc2\n", "README.md": "r\n"})
    assert result["mode"] == "subset"
    assert set(result["tests"]) == set(ALWAYS)


def test_uncovered_binary_file_selects_nothing_extra(repo):
    result = run(repo, {"crates/ui/src/unused.rs": "fn d2() {}\n"})
    assert set(result["tests"]) == set(ALWAYS)


@pytest.mark.parametrize(
    "changes",
    [
        {"Cargo.lock": "lock2\n"},
        {"crates/ui/Cargo.toml": "[package]\n"},
        {"crates/ui/build.rs": "fn main() {}\n"},
        {"test/remote-behavior/resources.py": "x = 2\n"},
        {"test/remote-behavior/selection.py": "x = 2\n"},
        {"crates/ui/src/brand_new.rs": "fn n() {}\n"},
        {"crates/ui/slint/app.slint": "x\n"},
        {"server/migrations/0099_x.sql": "x\n"},
    ],
    ids=[
        "lockfile",
        "manifest",
        "build-script",
        "test-infra",
        "selector",
        "unknown-source",
        "slint",
        "migration",
    ],
)
def test_unrecognized_change_falls_back_to_full(repo, changes):
    result = run(repo, changes)
    assert result["mode"] == "full"
    assert result["tests"] == []
    assert result["fallback"]


def test_missing_map_falls_back_to_full(repo):
    result = run(repo, {"crates/ui/src/download.rs": "fn b2() {}\n"}, repo / "absent.json")
    assert result["mode"] == "full"
    assert "missing" in result["fallback"]


def test_map_not_ancestor_of_head_is_stale(repo):
    git(repo, "checkout", "-q", "-b", "side")
    side = commit(repo, {"docs/side.md": "s\n"}, "side")
    git(repo, "checkout", "-q", "master")
    result = run(repo, {"crates/ui/src/download.rs": "fn b2() {}\n"}, write_map(repo, side))
    assert result["mode"] == "full"
    assert "stale" in result["fallback"]


# 地图之后、base 之前的改动同样让地图失真，必须算进挑选。
def test_changes_since_map_commit_are_included(repo):
    map_path = write_map(repo, git(repo, "rev-parse", "HEAD"))
    commit(repo, {SHARED: "fn c2() {}\n"}, "between")
    result = run(repo, {"docs/note.md": "doc2\n"}, map_path)
    assert set(result["tests"]) == {PICK, DOWNLOAD, *ALWAYS}


@pytest.mark.parametrize("fault", sorted(FAULTS))
def test_fault_anchor_selects_its_sensitivity_case(repo, fault):
    result = run(repo, {FAULTS[fault][0]: "fn fault2() {}\n"})
    assert f"test_remote.py::test_fault_sensitivity[{fault}]" in result["tests"]
    others = {f for f in FAULTS if FAULTS[f][0] != FAULTS[fault][0]}
    assert not {f"test_remote.py::test_fault_sensitivity[{f}]" for f in others} & set(
        result["tests"]
    )


def test_fault_sensitivity_skipped_without_anchor_change(repo):
    result = run(repo, {"crates/ui/src/download.rs": "fn b2() {}\n"})
    assert not [t for t in result["tests"] if "fault_sensitivity" in t]


def test_mutation_driver_change_selects_every_fault(repo):
    result = run(repo, {"test/remote-behavior/mutate.py": "x = 2\n"})
    assert result["mode"] == "subset"
    for fault in FAULTS:
        assert f"test_remote.py::test_fault_sensitivity[{fault}]" in result["tests"]


def test_scenario_module_change_selects_whole_module(repo):
    result = run(repo, {"test/remote-behavior/test_download.py": "x = 2\n"})
    assert result["mode"] == "subset"
    assert "test_download.py" in result["tests"]
