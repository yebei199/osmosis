"""验收分流入口必须拒绝无目标子集，避免零用例假绿。"""

import subprocess
from pathlib import Path


def test_acceptance_subset_requires_tests():
    """命令入口的参数错误必须非零，不能默默执行全量。"""
    result = subprocess.run(
        ["bun", str(Path(__file__).with_name("accept-run.ts")), "subset"],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 2
    assert "requires tests" in result.stderr


def test_unselected_suites_have_empty_results_without_erasing_selected_tests(tmp_path):
    """累计 checker 要求所有声明文件存在，未选中套件写零用例而非伪造通过。"""
    import os
    import shutil
    import sys
    import xml.etree.ElementTree as ET

    root = tmp_path / "repo"
    suite = root / "test/mcp-e2e"
    suite.mkdir(parents=True)
    shutil.copyfile(Path(__file__).with_name("accept-run.ts"), suite / "accept-run.ts")
    (root / "acceptance").mkdir()
    (root / "acceptance/run.toml").write_text(
        'junit=["test/mcp-e2e/results/junit.xml", "remote/results.xml"]\n'
    )
    binary = tmp_path / "bin"
    binary.mkdir()
    uv = binary / "uv"
    uv.write_text(
        f"#!{sys.executable}\n"
        + "import pathlib,sys\n"
        + "p=pathlib.Path(next(a.split('=',1)[1] for a in sys.argv if a.startswith('--junitxml=')))\n"
        + "p.parent.mkdir(parents=True,exist_ok=True)\n"
        + "p.write_text('<testsuites><testsuite><testcase name=\"selected\"/></testsuite></testsuites>')\n"
    )
    uv.chmod(0o700)
    result = subprocess.run(
        ["bun", str(suite / "accept-run.ts"), "subset", "test_client::selected"],
        env=dict(os.environ, PATH=str(binary) + os.pathsep + os.environ["PATH"]),
        capture_output=True,
        text=True,
        check=False,
        timeout=30,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert len(ET.parse(suite / "results/junit.xml").findall(".//testcase")) == 1
    assert not ET.parse(root / "remote/results.xml").findall(".//testcase")
