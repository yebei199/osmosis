"""公共查询的句柄生命周期与导航时的播放抽屉约束。"""

from unittest.mock import Mock

import pytest

import ui as controls
from ui import UI


def navigation_ui(drawer_open=True, close_works=True):
    ui = UI.__new__(UI)
    ui.window = "window"
    state = {"drawer": drawer_open, "destination": "daily"}

    def find(element_id, label=None, prefix=False):
        if element_id == "SideNavRow::touch" and label == "已下载":
            return "downloads"
        if element_id == "RoundControl::touch":
            if label == "收起更多" and state["drawer"]:
                return "close"
            if label == "更多" and not state["drawer"]:
                return "more"
        return None

    def activate(handle, **kwargs):
        if handle == "close" and close_works:
            state["drawer"] = False
        if handle == "downloads":
            if state["drawer"]:
                state["drawer"] = False
            else:
                state["destination"] = "downloads"

    ui.find = Mock(side_effect=find)
    ui.activate = Mock(side_effect=activate)
    ui.call = Mock(return_value={"accessibleChecked": False})
    return ui, state


@pytest.mark.parametrize("drawer_open", [True, False])
def test_destination_reaches_navigation_with_or_without_drawer(drawer_open):
    ui, state = navigation_ui(drawer_open)
    ui.destination("已下载")
    assert state == {"drawer": False, "destination": "downloads"}
    if drawer_open:
        assert ui.activate.call_args_list[0].args == ("close",)
        assert ui.activate.call_args_list[0].kwargs == {"pointer": True}


def test_destination_does_not_navigate_before_drawer_is_confirmed_closed(monkeypatch):
    ui, state = navigation_ui(close_works=False)
    original_wait = controls.wait_until
    monkeypatch.setattr(
        controls,
        "wait_until",
        lambda predicate, label: original_wait(predicate, label, timeout=0.01),
    )
    with pytest.raises(TimeoutError, match="playback drawer closed"):
        ui.destination("已下载")
    assert state == {"drawer": True, "destination": "daily"}
    assert all(call.args != ("downloads",) for call in ui.activate.call_args_list)


def query_ui(properties, query_error=None, action_error=None):
    instance = UI.__new__(UI)
    instance.root = "root"
    scans = []

    def call(tool, **arguments):
        if tool == "query_element_descendants":
            assert arguments["elementHandle"] == "root"
            if query_error is not None:
                raise query_error
            scans.append(arguments)
            return {"elementHandles": ["old" if len(scans) == 1 else "fresh"]}
        if tool == "get_element_properties":
            item = next(properties)
            if isinstance(item, Exception):
                raise item
            return item
        if tool == "invoke_accessibility_action":
            if action_error is not None:
                raise action_error
            return {}
        raise AssertionError(f"unexpected tool: {tool}")

    instance.call = Mock(side_effect=call)
    return instance, scans


def visible_properties(label="下载这一首", width=100, x=10):
    return {
        "accessibleLabel": label,
        "absolutePosition": {"x": x, "y": 20},
        "size": {"width": width, "height": 30},
    }


# 属性读取失效才重新扫描；must 和普通 find 走同一恢复路径，成功后只点击一次。
@pytest.mark.parametrize("message", ["element that was destroyed", "Invalid handle"])
@pytest.mark.parametrize("method", ["find", "must"])
def test_property_handle_expiration_rescans_root_once(message, method):
    instance, scans = query_ui(iter([RuntimeError(message), visible_properties()]))
    handle = getattr(instance, method)("DrawerRow::touch", "下载这一首")
    assert handle == "fresh"
    instance.activate(handle)
    assert len(scans) == 2
    reads = [
        c.kwargs["elementHandle"]
        for c in instance.call.call_args_list
        if c.args == ("get_element_properties",)
    ]
    assert reads == ["old", "fresh"]
    actions = [
        c for c in instance.call.call_args_list if c.args == ("invoke_accessibility_action",)
    ]
    assert len(actions) == 1
    assert actions[0].kwargs["elementHandle"] == "fresh"


# 原实现读标签后会为可见性再读一次；同一快照消除这次句柄失效窗口。
def test_visibility_uses_the_label_property_snapshot():
    instance, scans = query_ui(
        iter([visible_properties(), RuntimeError("element that was destroyed")])
    )
    assert instance.find("DrawerRow::touch", "下载这一首") == "old"
    assert len(scans) == 1
    assert sum(c.args == ("get_element_properties",) for c in instance.call.call_args_list) == 1


# 第二次扫描仍失效就传播原错误，不让 must 的轮询无限续命。
def test_persistent_expiration_fails_after_one_rescan():
    last = RuntimeError("Invalid handle")
    instance, scans = query_ui(iter([RuntimeError("element that was destroyed"), last]))
    with pytest.raises(RuntimeError) as raised:
        instance.must("DrawerRow::touch", "下载这一首")
    assert raised.value is last
    assert len(scans) == 2


@pytest.mark.parametrize(
    "error",
    [
        RuntimeError("invalid RPC arguments"),
        RuntimeError("element destroyed in an unrelated RPC"),
        ConnectionError("bridge failed"),
        ValueError("malformed reply"),
    ],
)
def test_other_property_errors_propagate_without_rescan(error):
    instance, scans = query_ui(iter([error]))
    with pytest.raises(type(error)) as raised:
        instance.find("DrawerRow::touch")
    assert raised.value is error
    assert len(scans) == 1


# 恢复只针对属性读取，不重试根查询错误或已经发出的动作。
def test_query_error_is_not_retried():
    error = RuntimeError("Invalid handle")
    instance, _ = query_ui(iter([]), query_error=error)
    with pytest.raises(RuntimeError) as raised:
        instance.find("DrawerRow::touch")
    assert raised.value is error
    instance.call.assert_called_once()


def test_expired_click_is_not_replayed():
    error = RuntimeError("element that was destroyed")
    instance, scans = query_ui(
        iter([visible_properties(), visible_properties()]), action_error=error
    )
    handle = instance.find("DrawerRow::touch", "下载这一首")
    with pytest.raises(RuntimeError) as raised:
        instance.activate(handle)
    assert raised.value is error
    assert len(scans) == 1
    assert (
        sum(c.args == ("invoke_accessibility_action",) for c in instance.call.call_args_list) == 1
    )


# 保持原有精确/前缀标签和可见几何筛选，不因恢复机制放宽匹配。
@pytest.mark.parametrize(
    "label,prefix,width,x,wanted",
    [
        ("下载这一首", False, 100, 10, "old"),
        ("下载", True, 100, 10, "old"),
        ("其他", False, 100, 10, None),
        ("下载这一首", False, 0, 10, None),
        ("下载这一首", False, 100, 2000, None),
        ("下载这一首", False, 100, -1, None),
    ],
)
def test_find_preserves_label_and_visibility_filters(label, prefix, width, x, wanted):
    properties = visible_properties(width=width, x=x)
    instance, scans = query_ui(iter([properties, properties]))
    assert instance.find("DrawerRow::touch", label, prefix) == wanted
    assert len(scans) == 1


@pytest.mark.parametrize("failed", [False, True])
def test_mcp_reply_is_recorded_before_result_or_error(tmp_path, monkeypatch, failed):
    import json
    from types import SimpleNamespace

    instance = UI.__new__(UI)
    instance.client = SimpleNamespace(directory=tmp_path, process=Mock(), app_pid=123, env={})
    reply = (
        {"error": {"message": "Invalid handle"}}
        if failed
        else {"result": {"content": [{"text": '{"accessibleChecked": true}'}]}}
    )
    monkeypatch.setattr(
        controls.subprocess,
        "run",
        Mock(return_value=SimpleNamespace(returncode=0, stdout=json.dumps(reply))),
    )
    if failed:
        with pytest.raises(RuntimeError, match="Invalid handle"):
            instance.call("get_element_properties", elementHandle="more")
    else:
        assert instance.call("get_element_properties", elementHandle="more") == {
            "accessibleChecked": True
        }
    entry = json.loads((tmp_path / "mcp-responses.jsonl").read_text())
    assert entry["tool"] == "get_element_properties"
    assert entry["arguments"] == {"elementHandle": "more"}
    assert entry["reply"] == reply
