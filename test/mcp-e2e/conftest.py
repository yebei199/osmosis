"""本目录注册公共插件与协议测试 fixture。"""

from test_client import endpoint  # noqa: F401

# 让外部用例也可以显式通过 -p mcp_pytest 使用。
pytest_plugins = ["mcp_pytest"]


import pytest

from local_app import LocalApp


@pytest.fixture
def local_app(request):
    """每个真实用例独占应用与数据，消除上一用例的播放与视图状态。"""
    import tempfile
    from pathlib import Path

    base = Path(request.config.getoption("--mcp-artifacts"))
    base.mkdir(parents=True, exist_ok=True)
    app = LocalApp(Path(tempfile.mkdtemp(prefix="app-", dir=base)).resolve())
    with app.resources:
        try:
            yield app.start()
        finally:
            app.client.capture(app.directory / "final-state")


@pytest.fixture
def scenario_env(request):
    """可选真实上游模式使用调用者环境，默认只认隔离库。"""
    import os

    if request.config.getoption("--mcp-live"):
        return os.environ
    return request.getfixturevalue("local_app").environment
