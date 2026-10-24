"""pytest 失败自动采集，不改变用例断言或自动接受基准。"""

import hashlib
import shutil
import tempfile
from pathlib import Path

import pytest

from mcp_client import Client


def pytest_addoption(parser):
    """应用端点及产物路径由调用者显式提供。"""
    parser.addoption("--mcp-url", default="http://127.0.0.1:8091/mcp")
    parser.addoption("--mcp-artifacts", default="results/mcp-failures")


@pytest.fixture
def mcp(request):
    """每个用例独享客户端状态。"""
    return Client(request.config.getoption("--mcp-url"))


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_makereport(item, call):
    """保留原始失败，另外采集截图、树及已有差异图。"""
    outcome = yield
    report = outcome.get_result()
    client = item.funcargs.get("mcp")
    if not report.failed or client is None:
        return
    identifier = hashlib.sha256(item.nodeid.encode()).hexdigest()[:16]
    base = Path(item.config.getoption("--mcp-artifacts"))
    base.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix=f"{identifier}-", dir=base))
    report.sections.append(("MCP artifacts", str(directory)))
    client.capture(directory)
    if client.last_diff and client.last_diff.is_file():
        shutil.copyfile(client.last_diff, directory / "diff.png")
