"""临时基准证明视觉比较与 pytest 自动失败产物的完整链路。"""

import os
import subprocess
import sys
from pathlib import Path

import pytest
from PIL import Image

from visual import compare, update_baseline


@pytest.mark.parametrize("delta,changed,passes", [(8, 2, True), (9, 1, True), (9, 2, False)])
def test_pixel_threshold_boundary(tmp_path, delta, changed, passes):
    """严格大于单通道 8 且占比严格大于千分之一才失败。"""
    baseline = tmp_path / "baseline.png"
    actual = tmp_path / "actual.png"
    Image.new("RGB", (1000, 1), "black").save(baseline)
    image = Image.new("RGB", (1000, 1), "black")
    for x in range(changed):
        image.putpixel((x, 0), (delta, 0, 0))
    image.save(actual)
    if passes:
        compare(actual, baseline, tmp_path / "diff.png")
    else:
        with pytest.raises(AssertionError, match="pixel"):
            compare(actual, baseline, tmp_path / "diff.png")
        assert (tmp_path / "diff.png").is_file()


def test_baseline_requires_explicit_update_and_rejects_size(tmp_path):
    """缺图和尺寸变化不能自动接受；更新是独立显式操作。"""
    actual, baseline = tmp_path / "actual.png", tmp_path / "baseline.png"
    Image.new("RGB", (3, 3), "black").save(actual)
    with pytest.raises(AssertionError, match="baseline"):
        compare(actual, baseline, tmp_path / "diff.png")
    update_baseline(actual, baseline)
    compare(actual, baseline, tmp_path / "diff.png")
    Image.new("RGB", (4, 3), "black").save(actual)
    with pytest.raises(AssertionError, match="size"):
        compare(actual, baseline, tmp_path / "diff.png")
    assert (tmp_path / "diff.png").exists()


def test_broken_element_produces_all_failure_artifacts(endpoint, tmp_path):
    """真实 pytest 子进程故意破坏截图，退出非零并自动保存三份产物。"""
    url, _ = endpoint
    baseline = tmp_path / "baseline.png"
    Image.new("RGB", (10, 10), "white").save(baseline)
    case = tmp_path / "test_broken.py"
    case.write_text(
        f"def test_broken(mcp, tmp_path):\n    mcp.expect_screenshot({str(baseline)!r}, tmp_path)\n"
    )
    artifacts = tmp_path / "artifacts"
    env = dict(os.environ, PYTHONPATH=str(Path(__file__).parent.resolve()))
    result = subprocess.run(
        [
            sys.executable,
            "-m",
            "pytest",
            "-p",
            "mcp_pytest",
            str(case),
            "--mcp-url",
            url,
            "--mcp-artifacts",
            str(artifacts),
            "-q",
        ],
        check=False,
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
    )
    assert result.returncode == 1, result.stdout + result.stderr
    assert list(artifacts.rglob("screenshot.png"))
    assert list(artifacts.rglob("element-tree.json"))
    assert list(artifacts.rglob("diff.png"))
