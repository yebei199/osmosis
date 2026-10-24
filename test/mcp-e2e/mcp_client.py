"""Slint MCP 连接、动态选择器及可观察结果断言；点击只发送一次。"""

import base64
import json
import time
import urllib.error
import urllib.request
from collections.abc import Callable
from http.client import HTTPResponse
from pathlib import Path
from urllib.parse import urlsplit


class McpError(RuntimeError):
    """保留协议或工具错误，禁止视作空选择器。"""


class StaleHandle(McpError):
    """只读查询遇到元素重建时允许重新定位。"""


def expect(
    probe: Callable[[], object],
    *,
    equals: object = True,
    timeout: float = 30,
    interval: float = 0.2,
) -> object:
    """轮询外部真相源，超时报告最后值；不自动重发用户动作。"""
    deadline = time.monotonic() + timeout
    observed = None
    while True:
        try:
            observed = probe()
        except StaleHandle as error:
            observed = error
        if observed == equals:
            return observed
        if time.monotonic() >= deadline:
            raise AssertionError(f"expected {equals!r}; observed {observed!r}")
        time.sleep(interval)


def read_message(response: HTTPResponse) -> str:
    """读取一条 JSON/SSE 消息，持续 SSE 连接在完整事件后即可返回。"""
    if response.headers.get_content_type() == "text/event-stream":
        data = []
        for encoded_line in response:
            line = encoded_line.decode().rstrip("\r\n")
            if not line and data:
                break
            if line.startswith("data:"):
                data.append(line[5:].lstrip())
        raw = "\n".join(data)
    else:
        raw = response.read().decode()
    if raw.lstrip().startswith(("event:", "data:", ":")):
        messages = [
            "\n".join(line[5:].lstrip() for line in event.splitlines() if line.startswith("data:"))
            for event in raw.split("\n\n")
        ]
        raw = next(message for message in messages if message)
    return raw


class Client:
    """连接调用者指定的应用，不隐式启动进程或改变环境。"""

    def __init__(
        self, url: str = "http://127.0.0.1:8091/mcp", *, timeout: float = 30, interval: float = 0.2
    ) -> None:
        """保存有界请求及轮询参数。"""
        if timeout <= 0 or interval <= 0:
            raise ValueError("timeout and interval must be positive")
        self.url, self.timeout, self.interval = url, timeout, interval
        # 本地 MCP 不经过机器上的 HTTP 代理，远端仍尊重调用者环境。
        self.opener = (
            urllib.request.build_opener(urllib.request.ProxyHandler({}))
            if urlsplit(url).hostname in {"127.0.0.1", "localhost", "::1"}
            else urllib.request.build_opener()
        )
        self.last_diff = None

    def content(self, name: str, **arguments: object):
        """保留图片块；兼容单响应 JSON 与标准 SSE data 消息。"""
        body = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }
        ).encode()
        request = urllib.request.Request(
            self.url,
            body,
            headers={
                "Content-Type": "application/json",
                "Accept": "application/json, text/event-stream",
            },
        )
        try:
            with self.opener.open(request, timeout=self.timeout) as response:
                raw = read_message(response)
            reply = json.loads(raw)
            if reply.get("error"):
                raise McpError(f"{name}: {reply['error']}")
            result = reply["result"]
            if result.get("isError"):
                message = str(result)
                if "Invalid handle" in message or "element that was destroyed" in message:
                    raise StaleHandle(f"{name}: {result}")
                raise McpError(f"{name}: {result}")
            return result["content"]
        except (
            urllib.error.URLError,
            TimeoutError,
            OSError,
            ValueError,
            KeyError,
            TypeError,
            StopIteration,
        ) as error:
            raise McpError(f"{name}: invalid MCP response: {error}") from error

    def call(self, name: str, **arguments: object):
        """解码工具文本 JSON，空响应或非 JSON 文本均报错。"""
        blocks = self.content(name, **arguments)
        try:
            return json.loads(next(block["text"] for block in blocks if block["type"] == "text"))
        except (ValueError, KeyError, StopIteration) as error:
            raise McpError(f"{name}: missing or invalid JSON text: {blocks}") from error

    def window(self):
        """不缓存窗口句柄，防止应用重建后引用失效。"""
        windows = self.call("list_windows")["windowHandles"]
        if len(windows) != 1:
            raise McpError(f"expected one window, got {len(windows)}")
        return windows[0]

    def elements(self, query):
        """从最新窗口根查动态 if/for 元素。"""
        root = self.call("get_window_properties", windowHandle=self.window())["rootElementHandle"]
        return (
            self.call(
                "query_element_descendants", elementHandle=root, findAll=True, queryStack=query
            ).get("elementHandles")
            or []
        )

    def by_id(self, element_id: str, index: int = 0) -> "Locator":
        """构建惰性 id 选择器，每次操作重新解析。"""
        return Locator(self, element_id=element_id, index=index)

    def by_label(self, label: str, index: int = 0) -> "Locator":
        """按完整无障碍标签选择，不依赖组件名称或位置。"""
        return Locator(self, label=label, index=index)

    def screenshot(self, destination: Path | str) -> None:
        """保存 MCP 返回的 PNG 图片块。"""
        blocks = self.content("take_screenshot", windowHandle=self.window())
        try:
            data = next(block["data"] for block in blocks if block["type"] == "image")
            Path(destination).write_bytes(base64.b64decode(data, validate=True))
        except (ValueError, KeyError, StopIteration) as error:
            raise McpError("screenshot: missing PNG image block") from error

    def capture(self, directory: Path | str) -> None:
        """独立采集树与截图，采集失败记录错误，不掩盖原始断言。"""
        directory = Path(directory)
        directory.mkdir(parents=True, exist_ok=True)
        errors = []
        for name, operation in [
            ("screenshot", lambda: self.screenshot(directory / "screenshot.png")),
            (
                "tree",
                lambda: (directory / "element-tree.json").write_text(
                    json.dumps(
                        self.call(
                            "get_element_tree",
                            elementHandle=self.call(
                                "get_window_properties", windowHandle=self.window()
                            )["rootElementHandle"],
                            maxElements=10000,
                        ),
                        ensure_ascii=False,
                    )
                ),
            ),
        ]:
            try:
                operation()
            except (McpError, OSError) as error:
                errors.append(f"{name}: {error}")
        if errors:
            (directory / "capture-errors.txt").write_text("\n".join(errors))

    def expect_screenshot(self, baseline: Path | str, directory: Path | str) -> None:
        """视觉断言保留差异路径，pytest 失败钩子负责汇总。"""
        from visual import compare

        directory = Path(directory)
        directory.mkdir(parents=True, exist_ok=True)
        actual = directory / "actual.png"
        self.screenshot(actual)
        self.last_diff = directory / "diff.png"
        compare(actual, Path(baseline), self.last_diff)


class Locator:
    """只保存查询条件，避免长期持有可失效的元素句柄。"""

    def __init__(
        self,
        client: Client,
        *,
        element_id: str | None = None,
        label: str | None = None,
        index: int = 0,
    ) -> None:
        """id 或标签二选一，index 用于稳定的曲目列表行。"""
        if index < 0:
            raise ValueError("index must be non-negative")
        self.client, self.element_id, self.label, self.index = client, element_id, label, index

    def all(self):
        """查询实时匹配元素；标签通过属性读取匹配。"""
        query = (
            [{"matchElementId": self.element_id}]
            if self.element_id
            else [{"matchDescendants": True}]
        )
        handles = self.client.elements(query)
        if self.label is not None:
            handles = [
                handle
                for handle in handles
                if self.client.call("get_element_properties", elementHandle=handle).get(
                    "accessibleLabel"
                )
                == self.label
            ]
        return handles

    def handle(self):
        """有界等待目标出现；只读查询允许重试。"""
        result = []

        def found():
            """每轮重新定位，保留最终命中的句柄。"""
            nonlocal result
            result = self.all()
            return len(result) > self.index

        expect(found, timeout=self.client.timeout, interval=self.client.interval)
        return result[self.index]

    def properties(self):
        """读取最新匹配元素属性。"""
        result = None

        def read():
            """读属性遇到失效句柄时重新从选择器开始。"""
            nonlocal result
            result = self.client.call("get_element_properties", elementHandle=self.handle())
            return True

        expect(read, timeout=self.client.timeout, interval=self.client.interval)
        return result

    def click(self):
        """只发一次点击，不因响应或用户状态延迟重复提交。"""
        return self.client.call("click_element", elementHandle=self.handle())

    def action(self, action: str):
        """卡墙经无障碍动作操作，不量坐标。"""
        return self.client.call(
            "invoke_accessibility_action", elementHandle=self.handle(), action=action
        )
