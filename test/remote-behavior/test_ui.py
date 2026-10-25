"""公共导航流程必须先通过真实控件收起播放抽屉。"""

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
