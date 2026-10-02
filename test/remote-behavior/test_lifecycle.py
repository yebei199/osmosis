"""F-002 轻量失败与中断验证；只起本测试持有的 Python 子进程。"""

import json
import os
import signal
import subprocess
import sys
from pathlib import Path

import pytest

from guardian import identity
from lifecycle import OwnedCommand, run_logged
from resources import World, wait_until

SUITE = Path(__file__).parent
SLEEPER = "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); print('ready', flush=True); time.sleep(120)"


def child_source(marker, status=None):
    tail = "time.sleep(120)" if status is None else f"sys.exit({status})"
    return f"""import json,subprocess,sys,time
from pathlib import Path
from guardian import identity
child = subprocess.Popen([sys.executable, '-c', {SLEEPER!r}], start_new_session=True, stdout=subprocess.PIPE)
assert child.stdout.readline() == b'ready\\n'
child.stdout.close()
Path({str(marker)!r}).write_text(json.dumps([child.pid, identity(child.pid)[1]]))
{tail}
"""


def assert_gone(marker):
    pid, birth = json.loads(marker.read_text())
    current = identity(pid)
    assert current is None or current[1] != birth, f"owned child still alive: {pid}/{birth}"


def assert_complete(path):
    record = json.loads(path.read_text())
    assert record["cleanup"]["complete"]
    assert record["cleanup"]["remaining"] == []
    return record


# leader 正常或失败退出都不代表 detached 子进程已经退出。
@pytest.mark.parametrize("status", [0, 7], ids=["success", "failure"])
def test_leader_exit_reaps_detached_child(tmp_path, status):
    marker = tmp_path / "child.json"
    owner = OwnedCommand(
        [sys.executable, "-c", child_source(marker, status)],
        tmp_path / "cleanup.json",
        env=dict(os.environ) | {"PYTHONPATH": str(SUITE)},
    )
    try:
        assert owner.wait(10) == status
        assert identity(json.loads(marker.read_text())[0]) is not None
    finally:
        owner.stop()
    assert owner.returncode == status
    assert_complete(tmp_path / "cleanup.json")
    assert_gone(marker)


# 真正嵌套 pytest 的 fixture 启动独立 session 后卡住；超时不是行为 RED。
def test_nested_pytest_timeout_cleans_world(tmp_path):
    marker = tmp_path / "child.json"
    inner = tmp_path / "test_inner.py"
    inner.write_text(f"""import sys,time
import pytest
from lifecycle import OwnedCommand
@pytest.fixture
def resource():
    owner = OwnedCommand([sys.executable, '-c', {child_source(marker)!r}],
                         {str(tmp_path / "inner-cleanup.json")!r})
    try:
        yield owner
    finally:
        owner.stop()
def test_blocked(resource):
    time.sleep(120)
""")
    env = dict(os.environ) | {"PYTHONPATH": str(SUITE)}
    status = run_logged(
        [
            sys.executable,
            "-m",
            "pytest",
            "-q",
            str(inner),
            "-o",
            "addopts=",
            "--junitxml",
            str(tmp_path / "inner.xml"),
        ],
        tmp_path,
        env,
        tmp_path,
        "nested",
        8,
    )
    assert marker.exists(), "nested fixture did not reach resource initialization"
    assert status == 124
    record = json.loads((tmp_path / "nested.json").read_text())
    assert record["reason"] == "timeout" and record["exit"] == 124
    assert_complete(tmp_path / "nested-cleanup.json")
    assert_gone(marker)


# run.sh 共用 shell 入口收到 TERM/INT，原状态、CLI 取消文件与清理结果须完整。
@pytest.mark.parametrize("number", [signal.SIGTERM, signal.SIGINT], ids=["term", "int"])
def test_outer_cancel_cleans_descendants(tmp_path, number):
    marker = tmp_path / "child.json"
    env = dict(os.environ) | {"PYTHONPATH": str(SUITE)}
    shell = """set -euo pipefail
suite_dir=$1
suite_python=$2
export REMOTE_BEHAVIOR_ARTIFACTS=$3
source "$suite_dir/lifecycle.sh"
run_owned outer 60 "$suite_python" -c "$4"
"""
    outer = OwnedCommand(
        [
            "bash",
            "-c",
            shell,
            "175-cancel-test",
            str(SUITE),
            sys.executable,
            str(tmp_path),
            child_source(marker),
        ],
        tmp_path / "test-custody.json",
        env=env,
    )
    try:
        wait_until(marker.exists, "cancellable command initialized", timeout=10)
        fd = os.pidfd_open(outer.pid)
        try:
            signal.pidfd_send_signal(fd, number)
        finally:
            os.close(fd)
        assert outer.wait(12) == 128 + number
    finally:
        outer.stop()
    record = json.loads((tmp_path / "outer.json").read_text())
    assert record["reason"] == "cancelled" and record["exit"] == 128 + number
    assert int((tmp_path / "exit.txt").read_text()) == 128 + number
    assert int((tmp_path / "outer.cancel").read_text()) == 128 + number
    assert int((tmp_path / "cancel-wait.txt").read_text()) == 128 + number
    assert (tmp_path / "outer.log").exists()
    assert_complete(tmp_path / "outer-cleanup.json")
    assert_gone(marker)


# World 初始化中途失败时，已登记资源仍由原 finally 路径回收。
def test_partial_initialization_cleans_registered_resource(tmp_path):
    world = World(SUITE.parents[1], tmp_path, Path("unused"), Path("unused"))
    world.env["PYTHONPATH"] = str(SUITE)
    marker = tmp_path / "child.json"
    try:
        world.spawn("first", [sys.executable, "-c", child_source(marker)])
        wait_until(marker.exists, "first resource initialized", timeout=10)
        with pytest.raises(RuntimeError, match="initialization failed"):
            world.spawn("second", [str(tmp_path / "missing-executable")])
    finally:
        world.close()
    record = json.loads((tmp_path / "resources.json").read_text())
    assert record["cleanup_errors"] == []
    assert_complete(tmp_path / "first-cleanup.json")
    assert_gone(marker)


# 不属于 guardian 的另一个持有进程必须活着；不按名称扩大清理范围。
def test_cleanup_preserves_unrelated_process(tmp_path):
    unrelated = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"])
    try:
        assert (
            run_logged(
                [sys.executable, "-c", "raise SystemExit(7)"],
                tmp_path,
                dict(os.environ),
                tmp_path,
                "failure",
                10,
            )
            == 7
        )
        assert unrelated.poll() is None
        assert_complete(tmp_path / "failure-cleanup.json")
    finally:
        unrelated.terminate()
        unrelated.wait(timeout=10)
