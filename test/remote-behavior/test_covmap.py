"""覆盖地图生成：真插桩一个小程序，按世界目录归到 pytest 节点。"""

import os
import subprocess
from pathlib import Path

import pytest

from covmap import build

SUITE = Path(__file__).resolve().parent
PROGRAM = """
fn used() -> u64 { std::hint::black_box(7) }
fn main() { if used() == 0 { other::never(); } std::thread::sleep(std::time::Duration::from_secs(30)); }
mod other;
"""


def git(repo, *args):
    subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True)


@pytest.fixture
def program(tmp_path):
    repo = tmp_path / "repo"
    (repo / "src").mkdir(parents=True)
    (repo / "src/main.rs").write_text(PROGRAM)
    (repo / "src/other.rs").write_text("pub fn never() {}\n")
    git(repo, "init", "-q")
    git(
        repo,
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "base",
    )
    binary = tmp_path / "program"
    subprocess.run(
        [
            str(SUITE / "coverage-rustc.sh"),
            "rustc",
            "--crate-name",
            "program",
            "src/main.rs",
            "-o",
            str(binary),
        ],
        cwd=repo,
        check=True,
    )
    return repo, binary


# 被 SIGKILL 的进程也要留下计数，世界的真实停止方式就是信号。
def run_world(binary, profiles, node):
    profiles.mkdir(parents=True)
    (profiles / "nodeid").write_text(node)
    env = os.environ | {"LLVM_PROFILE_FILE": str(profiles / "%p-%m%c.profraw")}
    child = subprocess.Popen([binary], env=env)
    try:
        child.wait(timeout=2)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait()


def junit(path, names):
    cases = "".join(f'<testcase classname="{c}" name="{n}"/>' for c, n in names)
    path.write_text(f"<testsuites><testsuite>{cases}</testsuite></testsuites>")
    return path


def test_map_records_executed_files_per_node(program, tmp_path):
    repo, binary = program
    run_world(binary, tmp_path / "profiles/cov-a", "test_remote.py::test_a")
    names = [("test_remote", "test_a"), ("test_remote", "test_plain"), ("test_position", "x")]
    data = build(tmp_path / "profiles", junit(tmp_path / "j.xml", names), [binary], repo)
    assert data["universe"] == ["src/main.rs", "src/other.rs"]
    assert data["scenarios"] == {"test_remote.py::test_a": ["src/main.rs"]}
    assert data["unmapped"] == ["test_remote.py::test_plain"]
    assert len(data["commit"]) == 40


def test_world_without_profiles_fails(program, tmp_path):
    repo, binary = program
    (tmp_path / "profiles/cov-a").mkdir(parents=True)
    (tmp_path / "profiles/cov-a/nodeid").write_text("test_remote.py::test_a")
    with pytest.raises(RuntimeError, match="no coverage"):
        build(tmp_path / "profiles", junit(tmp_path / "j.xml", []), [binary], repo)


def test_coverage_outside_junit_fails(program, tmp_path):
    repo, binary = program
    run_world(binary, tmp_path / "profiles/cov-a", "test_remote.py::test_nested")
    with pytest.raises(RuntimeError, match="absent from JUnit"):
        build(tmp_path / "profiles", junit(tmp_path / "j.xml", []), [binary], repo)


# 发版门只放行 last-green 恰好等于候选提交的情况。
@pytest.mark.parametrize("recorded", [None, "other", "head"])
def test_release_gate_requires_green_candidate(program, tmp_path, recorded):
    repo, _ = program
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo, check=True, capture_output=True, text=True
    ).stdout.strip()
    state = tmp_path / "state"
    state.mkdir()
    if recorded:
        (state / "last-green").write_text((head if recorded == "head" else "0" * 40) + "\n")
    gate = subprocess.run(
        ["bash", str(SUITE / "release-gate.sh")],
        cwd=repo,
        env=os.environ | {"OSMOSIS_NIGHTLY_STATE": str(state)},
        capture_output=True,
        text=True,
        check=False,
    )
    assert (gate.returncode == 0) == (recorded == "head"), gate.stderr
