"""按改动挑场景的判据，夹具是临时 git 仓库加一份样例地图。"""

import json
from pathlib import Path
import subprocess

import pytest

from mutate import FAULTS
from selection import ALWAYS, MAP, select

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


# 每个源文件是一个三行函数；改中间那行是函数体内的改动。
def body(call):
    return f"fn f() {{\n    {call}();\n}}\n"


@pytest.fixture
def repo(tmp_path):
    git(tmp_path, "init", "-q", "-b", "master")
    git(tmp_path, "config", "user.email", "t@example.invalid")
    git(tmp_path, "config", "user.name", "t")
    sources = {
        "crates/ui/src/sync/group.rs": body("a"),
        "crates/ui/src/download.rs": body("b"),
        SHARED: body("c"),
        "crates/ui/src/unused.rs": body("d"),
        "Cargo.lock": "lock\n",
        "test/remote-behavior/resources.py": "x = 1\n",
        "test/remote-behavior/test_download.py": "x = 1\n",
        "docs/note.md": "doc\n",
    }
    for path, _ in FAULTS.values():
        sources[path] = body("fault")
    for always in ALWAYS:
        sources["test/remote-behavior/" + always.split("::")[0]] = "x = 1\n"
    commit(tmp_path, sources, "base")
    return tmp_path


# 地图记录生成提交、二进制里每个文件的函数首末行和每个场景实际执行的文件；
# 和真实流程一样，生成之后单独提交进仓库。
def write_map(repo, commit_id):
    files = {p.relative_to(repo).as_posix() for p in repo.glob("*/*/src/**/*.rs")}
    files |= {path for path, _ in FAULTS.values()}
    data = {
        "commit": commit_id,
        "functions": {path: [[1, 3]] for path in sorted(files)},
        "scenarios": {
            PICK: ["crates/ui/src/sync/group.rs", SHARED],
            DOWNLOAD: ["crates/ui/src/download.rs", SHARED],
        },
    }
    commit(repo, {MAP: json.dumps(data)}, "map")


# map_commit 为 None 表示不在这里提交地图（缺地图，或测试已经自己提交过）。
def run(repo, changes, map_commit="base"):
    if map_commit is not None:
        write_map(repo, git(repo, "rev-parse", "HEAD") if map_commit == "base" else map_commit)
    base = git(repo, "rev-parse", "HEAD")
    head = commit(repo, changes, "change")
    return select(repo, base, head)


def test_only_covering_scenario_is_selected(repo):
    result = run(repo, {"crates/ui/src/download.rs": body("b2")})
    assert result["mode"] == "subset"
    assert set(result["tests"]) == {DOWNLOAD, *ALWAYS}
    assert any(r["file"] == "crates/ui/src/download.rs" for r in result["reasons"])


def test_shared_file_selects_every_covering_scenario(repo):
    result = run(repo, {SHARED: body("c2")})
    assert set(result["tests"]) == {PICK, DOWNLOAD, *ALWAYS}


def test_documentation_selects_only_cheap_checks(repo):
    result = run(repo, {"docs/note.md": "doc2\n", "README.md": "r\n"})
    assert result["mode"] == "subset"
    assert set(result["tests"]) == set(ALWAYS)


def test_uncovered_binary_file_selects_nothing_extra(repo):
    result = run(repo, {"crates/ui/src/unused.rs": body("d2")})
    assert set(result["tests"]) == set(ALWAYS)


@pytest.mark.parametrize(
    "changes",
    [
        {"Cargo.lock": "lock2\n"},
        {"crates/ui/Cargo.toml": "[package]\n"},
        {"crates/ui/build.rs": "fn main() {}\n"},
        {"test/remote-behavior/resources.py": "x = 2\n"},
        {"test/remote-behavior/selection.py": "x = 2\n"},
        {"crates/ui/src/brand_new.rs": body("n")},
        {"crates/ui/src/download.rs": "const LIMIT: u8 = 1;\n" + body("b")},
        {"crates/ui/src/download.rs": "fn g() {\n    b();\n}\n"},
        {"crates/ui/src/download.rs": body("b") + "fn extra() {}\n"},
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
        "outside-body-const",
        "signature",
        "new-function",
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
    result = run(repo, {"crates/ui/src/download.rs": body("b2")}, None)
    assert result["mode"] == "full"
    assert "missing" in result["fallback"]
    assert result["reasons"] == [
        {"file": "crates/ui/src/download.rs", "rule": result["fallback"], "full": True}
    ]


def test_map_not_ancestor_of_head_is_stale(repo):
    git(repo, "checkout", "-q", "-b", "side")
    side = commit(repo, {"docs/side.md": "s\n"}, "side")
    git(repo, "checkout", "-q", "master")
    result = run(repo, {"crates/ui/src/download.rs": body("b2")}, side)
    assert result["mode"] == "full"
    assert "stale" in result["fallback"]


# 地图之后、base 之前的改动同样让地图失真，必须算进挑选。
def test_changes_since_map_commit_are_included(repo):
    write_map(repo, git(repo, "rev-parse", "HEAD"))
    commit(repo, {SHARED: body("c2")}, "between")
    result = run(repo, {"docs/note.md": "doc2\n"}, None)
    assert set(result["tests"]) == {PICK, DOWNLOAD, *ALWAYS}


@pytest.mark.parametrize("fault", sorted(FAULTS))
def test_fault_anchor_selects_its_sensitivity_case(repo, fault):
    result = run(repo, {FAULTS[fault][0]: body("fault2")})
    assert f"test_remote.py::test_fault_sensitivity[{fault}]" in result["tests"]
    others = {f for f in FAULTS if FAULTS[f][0] != FAULTS[fault][0]}
    assert not {f"test_remote.py::test_fault_sensitivity[{f}]" for f in others} & set(
        result["tests"]
    )


def test_fault_sensitivity_skipped_without_anchor_change(repo):
    result = run(repo, {"crates/ui/src/download.rs": body("b2")})
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


# 函数体外只改注释不改变行为，不触发全量，按文件覆盖照常挑。
def test_comment_outside_body_is_inert(repo):
    result = run(repo, {"crates/ui/src/download.rs": "// note\n" + body("b")})
    assert result["mode"] == "subset"
    assert set(result["tests"]) == {DOWNLOAD, *ALWAYS}


def test_deleted_binary_file_falls_back_to_full(repo):
    write_map(repo, git(repo, "rev-parse", "HEAD"))
    base = git(repo, "rev-parse", "HEAD")
    git(repo, "rm", "-q", "crates/ui/src/unused.rs")
    git(repo, "commit", "-q", "-m", "drop")
    result = select(repo, base, git(repo, "rev-parse", "HEAD"))
    assert result["mode"] == "full"


# 退回全量时每个认不出的文件都要列出理由，不只第一个。
def test_full_fallback_lists_every_unrecognized_file(repo):
    result = run(repo, {"Cargo.lock": "lock2\n", "crates/ui/slint/app.slint": "x\n"})
    assert result["mode"] == "full"
    flagged = {r["file"] for r in result["reasons"] if r.get("full")}
    assert flagged == {"Cargo.lock", "crates/ui/slint/app.slint"}


# accept-select/1：只有这五个键；输出不带路径，同一对提交在别的检出里算出同一份。
def test_output_is_the_accept_select_contract(repo, tmp_path):
    result = run(repo, {"crates/ui/src/download.rs": body("b2"), "docs/note.md": "x\n"})
    assert list(result) == ["schema", "mode", "tests", "fallback", "reasons"]
    assert result["schema"] == "accept-select/1"
    assert str(repo) not in json.dumps(result)
    base, head = git(repo, "rev-parse", "HEAD~1"), git(repo, "rev-parse", "HEAD")
    clone = tmp_path / "clone"
    git(tmp_path, "clone", "-q", str(repo), str(clone))
    (clone / MAP).write_text("{}")
    assert select(clone, base, head) == result


# 门禁逐个核对 base..head 的改动文件：每个都要有 tests 或 full。
@pytest.mark.parametrize(
    "changes",
    [
        {"crates/ui/src/download.rs": body("b2"), "README.md": "r\n"},
        {"Cargo.lock": "lock2\n", "crates/ui/src/download.rs": body("b2")},
    ],
    ids=["subset", "full"],
)
def test_every_changed_file_has_a_verdict(repo, changes):
    result = run(repo, changes)
    verdicts = {r["file"] for r in result["reasons"] if "tests" in r or r.get("full")}
    assert set(changes) <= verdicts


RUN = Path(__file__).resolve().parents[2] / "acceptance" / "run.sh"


def run_sh(*args):
    return subprocess.run(["bash", RUN, *args], capture_output=True, text=True)


@pytest.mark.parametrize(
    ("given", "expected"),
    [
        ("test_remote::test_y", ["test_remote.py::test_y"]),
        ("test_remote::test_q[a-b]", ["test_remote.py::test_q[a-b]"]),
        ("app-core::blocks::t", []),
        ("ui::music::tests::group::t", []),
        ("server::bin/server::routes::t", []),
        ("test_a.py::test_b", ["test_a.py::test_b"]),
    ],
    ids=[
        "pytest-junit",
        "pytest-junit-param",
        "nextest",
        "nextest-ui",
        "nextest-server",
        "pytest-node",
    ],
)
def test_subset_accepts_junit_ids(given, expected):
    out = run_sh("subset-args", given)
    assert out.returncode == 0
    assert out.stdout.split() == expected


def test_subset_with_every_argument_skipped_exits_zero():
    out = run_sh("subset", "app-core::blocks::t", "ui-x::y")
    assert out.returncode == 0, out.stderr
    assert "nothing to run" in out.stdout
