"""#183 证据目录保留策略；只在 tmp_path 里造目录，不碰真实缓存。"""

import os
import subprocess
import time
from pathlib import Path

import pytest

SCRIPT = Path(__file__).parent / "retention.sh"
DAY = 86400


def run_dir(root, name, exit_code, age):
    path = root / f"175-rb.{name}"
    (path / "build" / "debug").mkdir(parents=True)
    (path / "build" / "debug" / "server").write_text("binary")
    (path / "junit.xml").write_text("<testsuites/>")
    if exit_code is not None:
        (path / "exit.txt").write_text(f"{exit_code}\n")
    stamp = time.time() - age
    os.utime(path, (stamp, stamp))
    return path


def retain(root, keep=None):
    env = dict(os.environ)
    env.pop("REMOTE_BEHAVIOR_KEEP_RUNS", None)
    if keep is not None:
        env["REMOTE_BEHAVIOR_KEEP_RUNS"] = str(keep)
    return subprocess.run(
        ["bash", str(SCRIPT), str(root)], env=env, capture_output=True, text=True, check=False
    )


def test_keeps_latest_runs_strips_success_build_keeps_failure(tmp_path):
    root = tmp_path / "artifacts"
    root.mkdir()
    old = [run_dir(root, f"ok{index}", 0, (10 - index) * DAY) for index in range(5)]
    failed = run_dir(root, "failed", 1, DAY)
    other = root / "keep-me"
    other.mkdir()
    outside = tmp_path / "outside.txt"
    outside.write_text("untouched")

    result = retain(root)

    assert result.returncode == 0, result.stderr
    remaining = sorted(path.name for path in root.glob("175-rb.*"))
    assert remaining == sorted([failed.name, old[4].name, old[3].name])
    for path in old[3:]:
        assert not (path / "build").exists()
        assert (path / "junit.xml").read_text() == "<testsuites/>"
    assert (failed / "build" / "debug" / "server").read_text() == "binary"
    assert other.is_dir()
    assert outside.read_text() == "untouched"


def test_keep_count_comes_from_environment(tmp_path):
    for index in range(4):
        run_dir(tmp_path, f"ok{index}", 0, (10 - index) * DAY)

    assert retain(tmp_path, keep=1).returncode == 0

    assert [path.name for path in tmp_path.glob("175-rb.*")] == ["175-rb.ok3"]


# 没有 exit.txt 且一天内动过，视为别人正在跑；更早的视为被强杀的残留。
def test_unfinished_recent_run_is_left_alone(tmp_path):
    for index in range(4):
        run_dir(tmp_path, f"ok{index}", 0, index * 60)
    running = run_dir(tmp_path, "running", None, 2 * DAY / 24)
    abandoned = run_dir(tmp_path, "abandoned", None, 3 * DAY)

    assert retain(tmp_path, keep=1).returncode == 0

    assert (running / "build" / "debug" / "server").exists()
    assert not abandoned.exists()
    assert sorted(path.name for path in tmp_path.glob("175-rb.*")) == [
        "175-rb.ok0",
        running.name,
    ]


def test_symlinked_run_is_not_followed(tmp_path):
    root = tmp_path / "artifacts"
    root.mkdir()
    target = tmp_path / "elsewhere"
    (target / "build").mkdir(parents=True)
    (root / "175-rb.link").symlink_to(target)
    run_dir(root, "ok", 0, 0)

    assert retain(root, keep=1).returncode == 0

    assert (target / "build").is_dir()


@pytest.mark.parametrize("root", ["", "/", "//"])
def test_refuses_empty_or_filesystem_root(root):
    result = retain(root)
    assert result.returncode != 0
    assert "refuse" in result.stderr


@pytest.mark.parametrize("keep", ["0", "-1", "x"])
def test_refuses_invalid_keep_count(tmp_path, keep):
    survivor = run_dir(tmp_path, "ok", 0, 0)
    result = retain(tmp_path, keep=keep)
    assert result.returncode != 0
    assert "REMOTE_BEHAVIOR_KEEP_RUNS" in result.stderr
    assert survivor.is_dir()
