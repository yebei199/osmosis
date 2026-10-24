"""本目录注册公共插件与协议测试 fixture。"""

from test_client import endpoint  # noqa: F401

# 让外部用例也可以显式通过 -p mcp_pytest 使用。
pytest_plugins = ["mcp_pytest"]
