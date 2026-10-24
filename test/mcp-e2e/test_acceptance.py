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
