"""通过随机端口协议服务检查公共库；不冒充真实产品验收。"""

import base64
import io
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest
from PIL import Image

from mcp_client import Client, McpError, expect


@pytest.fixture
def endpoint():
    """每个用例独占 HTTP 服务和状态，结束后等待线程退出。"""
    state = {"sse": False, "error": None, "queries": 0, "clicks": 0}
    stream = io.BytesIO()
    Image.new("RGB", (10, 10), "black").save(stream, format="PNG")
    state["image"] = base64.b64encode(stream.getvalue()).decode()

    class Handler(BaseHTTPRequestHandler):
        """实现必要的 MCP 边界，业务判定仍调用真实库。"""

        def log_message(self, *args):
            """禁止 HTTP 日志混入测试结果。"""

        def do_POST(self):
            """按工具返回协议数据，可注入 SSE 或错误。"""
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            name = body["params"]["name"]
            values = {
                "list_windows": {"windowHandles": [1]},
                "get_window_properties": {"rootElementHandle": 2},
                "get_element_properties": {"accessibleLabel": "Play"},
                "get_element_tree": {"elements": ["Play"]},
            }
            if name == "query_element_descendants":
                state["queries"] += 1
                values[name] = {"elementHandles": [3] if state["queries"] >= 2 else []}
            if name == "click_element":
                state["clicks"] += 1
                values[name] = {}
            content = [{"type": "text", "text": json.dumps(values.get(name, {}))}]
            if name == "take_screenshot":
                content = [{"type": "image", "data": state["image"], "mimeType": "image/png"}]
            result = {"content": content}
            if state["error"] == "tool":
                result["isError"] = True
            reply = {"jsonrpc": "2.0", "id": body["id"], "result": result}
            if state["error"] == "rpc":
                reply = {"error": {"message": "broken"}}
            raw = json.dumps(reply)
            if state["error"] == "malformed":
                raw = "bad json"
            if state["sse"]:
                raw = f"event: message\ndata: {raw}\n\n"
            self.send_response(200)
            self.end_headers()
            self.wfile.write(raw.encode())

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}/mcp", state
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


@pytest.mark.parametrize("sse", [False, True])
def test_dynamic_selectors_wait_and_click_once(endpoint, sse):
    """动态 id 与标签都从根查询；查找重试不重复点击。"""
    url, state = endpoint
    state["sse"] = sse
    client = Client(url, timeout=0.2, interval=0.001)
    client.by_id("TrackList::touch").click()
    assert state["queries"] >= 2
    assert state["clicks"] == 1
    assert client.by_label("Play").properties()["accessibleLabel"] == "Play"


@pytest.mark.parametrize("error", ["rpc", "tool", "malformed"])
def test_protocol_errors_are_not_empty_success(endpoint, error):
    """协议与工具错误必须保留失败，不转为空选择器。"""
    url, state = endpoint
    state["error"] = error
    with pytest.raises(McpError):
        Client(url).call("list_windows")


def test_expect_reports_last_observation():
    """断言超时包含最后的实际值，不静默通过。"""
    with pytest.raises(AssertionError, match="observed.*wrong"):
        expect(lambda: "wrong", equals="right", timeout=0.01, interval=0.001)


def test_failure_capture_writes_screenshot_and_tree(endpoint, tmp_path):
    """失败产物是真实协议截图字节和元素树。"""
    url, _ = endpoint
    Client(url).capture(tmp_path)
    assert Image.open(tmp_path / "screenshot.png").size == (10, 10)
    assert "Play" in (tmp_path / "element-tree.json").read_text()


def test_connection_failure_is_explicit():
    """未监听的本机端点不能变成空成功。"""
    import socket

    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
        with pytest.raises(McpError):
            Client(f"http://127.0.0.1:{port}/mcp", timeout=0.05).call("list_windows")


def test_capture_errors_preserve_original_failure(endpoint, tmp_path):
    """采集错误单独落盘，不导致原始断言被替换。"""
    url, state = endpoint
    state["error"] = "tool"
    Client(url).capture(tmp_path)
    assert "list_windows" in (tmp_path / "capture-errors.txt").read_text()
