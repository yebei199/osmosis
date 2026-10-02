"""通过现有 MCP 操作真实 Slint 控件；定位沿用项目已有 e2e 的元素 id。"""

import json
import subprocess
import sys
from pathlib import Path

from media import DURATION
from resources import wait_until

LABELS = {
    "login": "\u767b\u5f55",
    "profile": "\u4e2a\u4eba",
    "daily": "\u6bcf\u65e5\u63a8\u8350",
    "radio": "\u7535\u53f0",
    "pause": "\u6682\u505c",
    "resume": "\u64ad\u653e",
    "next": "\u4e0b\u4e00\u9996",
    "prev": "\u4e0a\u4e00\u9996",
    "leave": "\u9000\u51fa",
    "local": "\u8f93\u51fa\u5230 \u672c\u673a",
    "output": "\u8f93\u51fa\u5230 ",
    "join": "\u52a0\u5165 ",
    "refresh": "\u6362\u4e00\u6279\u63a8\u8350",
}


# namespace 内的 HTTP 请求保留生产 UI 事件分派与网络路径。
class UI:
    def __init__(self, client):
        self.client = client
        wait_until(self.ready, client.role + " MCP window")

    # 每次工具调用都校验 JSON-RPC 和工具错误，不吞掉元素定位失败。
    def call(self, name, **arguments):
        self.client.process.alive()
        safe_arguments = {key: value for key, value in arguments.items() if key != "value"}
        with (self.client.directory / "ui-actions.jsonl").open("a") as log:
            log.write(json.dumps({"tool": name, "arguments": safe_arguments}) + "\n")
        payload = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }
        )
        result = subprocess.run(
            [
                "nsenter",
                "--target",
                str(self.client.app_pid),
                "--user",
                "--net",
                "--mount",
                "--preserve-credentials",
                "--",
                sys.executable,
                str(Path(__file__).with_name("bridge.py")),
            ],
            input=payload,
            capture_output=True,
            text=True,
            timeout=12,
            check=False,
            env=self.client.env,
        )
        if result.returncode:
            with (self.client.directory / "mcp-bridge-errors.jsonl").open("a") as log:
                log.write(
                    json.dumps({"tool": name, "exit": result.returncode, "stderr": result.stderr})
                    + "\n"
                )
            raise ConnectionError(f"MCP bridge failed ({result.returncode}): {result.stderr}")
        reply = json.loads(result.stdout)
        if reply.get("error") or reply.get("result", {}).get("isError"):
            raise RuntimeError(f"MCP {name}: {reply}")
        return json.loads(reply["result"]["content"][0]["text"])

    # 服务未就绪只在初始化处轮询，已经启动后工具错误直接传播。
    def ready(self):
        try:
            windows = self.call("list_windows").get("windowHandles", [])
            if len(windows) != 1:
                return False
            self.window = windows[0]
            self.root = self.call("get_window_properties", windowHandle=self.window)[
                "rootElementHandle"
            ]
            return True
        except ConnectionError:
            return False

    # 每次从根查后代，支持 if/for 动态生成的真实控件。
    def elements(self, element_id):
        return self.call(
            "query_element_descendants",
            elementHandle=self.root,
            findAll=True,
            queryStack=[{"matchElementId": element_id}],
        ).get("elementHandles", [])

    # 几何只用于选屏内实例和控件相对拖动，不持久保存坐标。
    def visible(self, handle):
        p = self.call("get_element_properties", elementHandle=handle)
        x, y = p["absolutePosition"].get("x", 0), p["absolutePosition"].get("y", 0)
        size = p["size"]
        return size["width"] > 0 and size["height"] > 0 and 0 <= x < 1100 and 0 <= y < 900

    # 多个 PlayerBar 实例时限定屏内控件，并拒绝无匹配。
    def find(self, element_id, label=None, prefix=False):
        for handle in self.elements(element_id):
            properties = self.call("get_element_properties", elementHandle=handle)
            value = properties.get("accessibleLabel") or ""
            if label is not None and not (value.startswith(label) if prefix else value == label):
                continue
            if self.visible(handle):
                return handle
        return None

    # 必需入口缺失是初始化/测试设计失败，不能认作 RED。
    def must(self, element_id, label=None, prefix=False):
        return wait_until(lambda: self.find(element_id, label, prefix), f"UI {element_id}/{label}")

    # 列表行走真实命中分派，语义按钮走控件声明的无障碍动作。
    def activate(self, handle, pointer=False, action="Default_"):
        if pointer:
            self.call("click_element", elementHandle=handle)
        else:
            self.call("invoke_accessibility_action", elementHandle=handle, action=action)

    # 标签在 HoverButton 根，点击落在同一按钮的唯一可见 touch。
    def login_submit(self, page):
        buttons = self.call(
            "query_element_descendants",
            elementHandle=page,
            findAll=True,
            queryStack=[{"matchElementTypeName": "HoverButton"}],
        ).get("elementHandles", [])
        matches = []
        for button in buttons:
            properties = self.call("get_element_properties", elementHandle=button)
            if properties.get("accessibleLabel") == LABELS["login"] and self.visible(button):
                matches.append((button, properties))
        if len(matches) != 1:
            raise RuntimeError(f"login page requires one visible submit button: {len(matches)}")
        button, properties = matches[0]
        touches = self.call(
            "query_element_descendants",
            elementHandle=button,
            findAll=True,
            queryStack=[{"matchElementId": "HoverButton::touch"}],
        ).get("elementHandles", [])
        visible = [touch for touch in touches if self.visible(touch)]
        if len(visible) != 1:
            raise RuntimeError(f"login button requires one visible touch: {len(visible)}")
        touch = visible[0]
        (self.client.directory / "login-submit.json").write_text(
            json.dumps(
                {
                    "page": page,
                    "button": button,
                    "button_properties": properties,
                    "touch": touch,
                    "touch_properties": self.call("get_element_properties", elementHandle=touch),
                    "matching_buttons": len(matches),
                    "visible_touches": len(visible),
                },
                indent=2,
            )
        )
        return touch

    # 所有客户端在真实登录页提交隔离测试账户，不直接写登录态。
    def login(self, username, password):
        page = self.must("MainWindow::login-page")
        # 填值前保存原始子树，避免将口令或账户值写入证据。
        (self.client.directory / "login-tree-before-input.json").write_text(
            json.dumps(
                self.call("get_element_tree", elementHandle=page, maxElements=1000), indent=2
            )
        )
        user = self.must("LoginPage::username")
        secret = self.must("LoginPage::password")
        self.call("set_element_value", elementHandle=user, value=username)
        self.call("set_element_value", elementHandle=secret, value=password)
        self.activate(self.login_submit(page), pointer=True)
        wait_until(lambda: not self.elements("LoginPage::username"), "login completion")

    # Escape 沿用已有 pick-e2e 的收起动作，随后进入音乐主 tab。
    def music(self, section="daily"):
        self.call("dispatch_key_event", windowHandle=self.window, text="\x1b")
        navigation = self.elements("NavItem::touch")
        if len(navigation) < 2:
            raise RuntimeError("music navigation is absent")
        self.activate(navigation[1], pointer=True)
        handle = self.find("MusicRail::item-touch", LABELS[section])
        if not handle:
            handle = self.must("MusicBar::item-touch", LABELS[section])
        self.activate(handle)

    # 出声选择从个人页的同一 OutputChip 进入。
    def profile(self):
        self.call("dispatch_key_event", windowHandle=self.window, text="\x1b")
        handle = self.find("RoundControl::touch", LABELS["profile"])
        if not handle:
            navigation = self.elements("NavItem::touch")
            if len(navigation) < 3:
                raise RuntimeError("profile navigation is absent")
            handle = navigation[2]
        self.activate(handle, pointer=True)

    # 设备名使用本轮 hostname 前缀，真实客户端的 PID 后缀由 UI 提供。
    def output(self, client):
        self.profile()
        label = LABELS["local"] if client is self.client else LABELS["output"] + client.name + " #"
        self.activate(self.must("OutputChip::touch", label, client is not self.client))

    # 加入共同出声只用于断线场景，行为仍由真实 MemberToggle 触发。
    def join_output(self, client):
        self.profile()
        self.activate(self.must("MemberToggle::touch", LABELS["join"] + client.name + " #", True))

    # 列表和卡墙都从每日推荐进入，并保留用户各自的确认动作。
    def pick(self, mode, index=0):
        self.music()
        view = self.must(f"WallView::view-{mode}-btn")
        self.activate(view)
        if mode == "list":
            wait_until(lambda: len(self.elements("TrackList::touch")) > index, "daily rows")
            self.activate(self.elements("TrackList::touch")[index], pointer=True)
            return
        wall = self.must("WallView::wall-area")
        # 无 focus 时首次 Increment 选第一首，第二次才移到第二首。
        for _ in range(index + 1):
            self.activate(wall, action="Increment")
        self.activate(wall)

    # 控制条的可访问标签随真实播放状态改变。
    def transport(self, operation):
        try:
            handle = self.must("RoundControl::touch", LABELS[operation])
        except TimeoutError:
            tree = self.call("get_element_tree", elementHandle=self.root, maxElements=1000)
            (self.client.directory / "transport-failure-tree.json").write_text(
                json.dumps(tree, indent=2)
            )
            raise
        properties = self.call("get_element_properties", elementHandle=handle)
        with (self.client.directory / "transport-controls.jsonl").open("a") as log:
            log.write(json.dumps({"operation": operation, "properties": properties}) + "\n")
        self.activate(handle)

    # 在真实可见播放条子树同时核对曲目和播放态，保存原始树而非构造投影。
    def playback_entry(self, track_id, playing):
        def matching_tree():
            bars = self.call(
                "query_element_descendants",
                elementHandle=self.root,
                findAll=True,
                queryStack=[{"matchElementTypeName": "PlayerBar"}],
            ).get("elementHandles", [])
            visible = [bar for bar in bars if self.visible(bar)]
            if len(visible) != 1:
                return None
            tree = self.call("get_element_tree", elementHandle=visible[0], maxElements=1000)
            assert not tree["truncated"], "playback entry evidence truncated"
            labels = [element.get("accessibleLabel") for element in tree["elements"]]
            control = LABELS["pause" if playing else "resume"]
            return tree if f"RB-{track_id}" in labels and control in labels else None

        try:
            tree = wait_until(matching_tree, f"visible playback entry {track_id}/{playing}")
        except TimeoutError:
            tree = self.call("get_element_tree", elementHandle=self.root, maxElements=1000)
            (self.client.directory / "playback-entry-failure-tree.json").write_text(
                json.dumps(tree, indent=2)
            )
            raise
        with (self.client.directory / "playback-entry-trees.jsonl").open("a") as log:
            log.write(json.dumps({"track_id": track_id, "playing": playing, "tree": tree}) + "\n")

    # 真正空队列没有可见播放条，也没有播放/暂停按钮。
    def no_playback_entry(self):
        tree = self.call("get_element_tree", elementHandle=self.root, maxElements=1000)
        (self.client.directory / "empty-playback-tree.json").write_text(json.dumps(tree, indent=2))
        assert not tree["truncated"], "empty queue evidence truncated"
        assert not any(
            any(item.get("typeName") == "PlayerBar" for item in element["typeNamesAndIds"])
            for element in tree["elements"]
        ), "empty local queue presented a playback entry"
        assert not self.find("RoundControl::touch", LABELS["resume"])
        assert not self.find("RoundControl::touch", LABELS["pause"])

    # 成功恢复后用户不应继续看到请求失败横幅；音频判据先执行。
    def no_error_banner(self):
        assert not self.elements("MainWindow::banner"), "unexpected user-visible error banner"

    # seek 位置基于元素实时几何，进入 ProgressBar 的真实指针处理。
    def seek(self, seconds):
        handle = self.must("ProgressBar::seek-touch")
        p = self.call("get_element_properties", elementHandle=handle)
        point = p["absolutePosition"]
        self.call(
            "drag_element",
            elementHandle=handle,
            button="Left",
            target={
                "x": point.get("x", 0) + p["size"]["width"] * seconds / DURATION,
                "y": point.get("y", 0) + p["size"]["height"] / 2,
            },
        )

    # 打开电台分区；仍拥有暂停的 FM 批次时只展示列表，不自动继续。
    def radio(self):
        self.music("radio")

    # 展开常驻控制条的更多菜单,复用语义按钮定位开关。
    def playback_options(self):
        if not self.find("DrawerRow::touch", "随机播放"):
            self.activate(self.must("RoundControl::touch", "更多"))

    # 读取真实可访问状态,不以服务端的状态替代按钮显示。
    def shuffle_checked(self):
        self.playback_options()
        handle = self.must("DrawerRow::touch", "随机播放")
        return self.call("get_element_properties", elementHandle=handle)["accessibleChecked"]

    # 三态循环由控件无障碍标签给出,与用户看到的文字共用绑定。
    def loop_label(self):
        self.playback_options()
        handle = self.must("DrawerRow::touch", "循环: ", True)
        return self.call("get_element_properties", elementHandle=handle)["accessibleLabel"]

    # 控件动作走 Slint 的用户入口,不会直接发送组 HTTP 意图。
    def shuffle(self):
        self.playback_options()
        self.activate(self.must("DrawerRow::touch", "随机播放"))

    # 循环一次仍让生产回调根据真实组状态计算下一态。
    def cycle_loop(self):
        self.playback_options()
        self.activate(self.must("DrawerRow::touch", "循环: ", True))

    # 退出播放组使用常驻横幅的用户入口。
    def leave(self):
        self.activate(self.must("MainWindow::exit-touch", LABELS["leave"]))
