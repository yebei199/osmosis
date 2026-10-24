"""通过随机端口协议服务检查公共库；不冒充真实产品验收。"""

import base64
import io
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import NotRequired, TypedDict

import pytest
from PIL import Image

from mcp_client import Client, McpError, expect


class ProtocolState(TypedDict):
    """协议替身的可变观察状态，每个 fixture 独占。"""

    sse: bool
    error: str | None
    queries: int
    clicks: int
    image: str
    stale: NotRequired[bool]
    duplicate: NotRequired[bool]
    release: NotRequired[threading.Event]
    requests: list[dict[str, object]]


@pytest.fixture
def endpoint():
    """每个用例独占 HTTP 服务和状态，结束后等待线程退出。"""
    state: ProtocolState = {
        "sse": False,
        "error": None,
        "queries": 0,
        "clicks": 0,
        "image": "",
        "requests": [],
    }
    stream = io.BytesIO()
    Image.new("RGB", (10, 10), "black").save(stream, format="PNG")
    state["image"] = base64.b64encode(stream.getvalue()).decode()

    class Handler(BaseHTTPRequestHandler):
        """实现必要的 MCP 边界，业务判定仍调用真实库。"""

        def log_message(self, format: str, *args: object) -> None:
            """禁止 HTTP 日志混入测试结果。"""

        def do_POST(self):
            """按工具返回协议数据，可注入 SSE 或错误。"""
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            name = body["params"]["name"]
            state["requests"].append(body["params"])
            values = {
                "list_windows": {"windowHandles": [1]},
                "get_window_properties": {"rootElementHandle": 2},
                "get_element_properties": {"accessibleLabel": "Play", "accessibleRole": "Button"},
                "get_element_tree": {"elements": ["Play"]},
            }
            if name == "query_element_descendants":
                state["queries"] += 1
                values[name] = {
                    "elementHandles": ([3, 3] if state.get("duplicate") else [3])
                    if state["queries"] >= 2
                    else []
                }
            if name == "click_element":
                state["clicks"] += 1
                values[name] = {}
            content = [{"type": "text", "text": json.dumps(values.get(name, {}))}]
            if name == "take_screenshot":
                content = [{"type": "image", "data": state["image"], "mimeType": "image/png"}]
            result: dict[str, object] = {"content": content}
            if name == "get_element_properties" and state.get("stale"):
                state["stale"] = False
                result = {
                    "isError": True,
                    "content": [
                        {
                            "type": "text",
                            "text": "Element handle refers to element that was destroyed",
                        }
                    ],
                }
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
            if state["sse"]:
                self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            self.wfile.write(raw.encode())
            self.wfile.flush()
            release = state.get("release")
            if release is not None:
                release.wait(timeout=2)

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
    assert any(
        request["arguments"].get("queryStack") == [{"matchElementId": "TrackList::touch"}]
        for request in state["requests"]
    )
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


def test_pixel_source_replaces_mcp_screenshot(endpoint, tmp_path):
    """设了像素来源（无头会话的合成器输出）就用它，视觉断言和失败产物都走这条。"""
    url, _ = endpoint
    client = Client(url)
    client.pixels = lambda destination: Image.new("RGB", (7, 5), "red").save(destination)
    client.capture(tmp_path)
    assert Image.open(tmp_path / "screenshot.png").size == (7, 5)
    client.pixels = lambda destination: (_ for _ in ()).throw(OSError("grim gone"))
    with pytest.raises(McpError, match="pixel source"):
        client.screenshot(tmp_path / "broken.png")


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


def test_stale_read_requeries_but_does_not_replay_click(endpoint):
    """元素重建时只重新读取，点击仍只发送一次。"""
    url, state = endpoint
    state["stale"] = True
    client = Client(url, timeout=1, interval=0.001)
    assert client.by_label("Play").properties()["accessibleLabel"] == "Play"
    client.by_id("TrackList::touch").click()
    assert state["clicks"] == 1


def test_loopback_ignores_http_proxy(endpoint, monkeypatch):
    """机器代理配置不能把本机 MCP 请求送向外网。"""
    url, _ = endpoint
    monkeypatch.setenv("http_proxy", "http://127.0.0.1:1")
    monkeypatch.setenv("no_proxy", "")
    assert Client(url).call("list_windows")["windowHandles"] == [1]


def test_missing_label_times_out_instead_of_clicking_other_element(endpoint):
    """错误标签不能退回列表首项误点。"""
    url, state = endpoint
    client = Client(url, timeout=0.05, interval=0.001)
    with pytest.raises(AssertionError):
        client.by_label("Missing").click()
    assert state["clicks"] == 0


def test_sse_returns_result_without_waiting_for_stream_close(endpoint):
    """SSE 工具结果应在事件结束时返回，不等待持续连接关闭。"""
    url, state = endpoint
    state["sse"] = True
    release = threading.Event()
    state["release"] = release
    try:
        assert Client(url, timeout=0.2).call("list_windows")["windowHandles"] == [1]
    finally:
        release.set()


def test_duplicate_handles_are_one_element(endpoint):
    """真实递归查询会重复返回句柄，index 必须对应不同元素。"""
    url, state = endpoint
    state["duplicate"] = True
    state["queries"] = 2
    client = Client(url)
    assert client.by_id("TrackList::touch").all() == [3]
    assert client.by_label("Play").all() == [3]


def test_label_selector_can_require_button_role(endpoint):
    """登录标题和提交按钮可同名，调用者必须能限定交互角色。"""
    url, state = endpoint
    client = Client(url)
    client.by_label("Play", role="Button").click()
    assert state["clicks"] == 1
