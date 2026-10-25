"""观察等待必须保留命中的快照，不在返回后重新读取动态状态。"""

from unittest.mock import Mock

import pytest

import resources
import test_dislike as dislike


@pytest.mark.parametrize("expected", [["ready"], [], {}, 0, False, None])
def test_wait_returns_the_matching_snapshot_without_another_read(expected):
    read = Mock(side_effect=[object(), expected, object()])
    observed = resources.wait_for_snapshot(read, lambda value: value is expected, "snapshot")
    assert observed is expected
    assert read.call_count == 2


def test_snapshot_wait_still_times_out_when_nothing_matches():
    read = Mock(return_value=["old"])
    with pytest.raises(TimeoutError, match="not ready: current rows"):
        resources.wait_for_snapshot(
            read, lambda value: value == ["new"], "current rows", timeout=0.001
        )
    assert read.call_count >= 1


@pytest.mark.parametrize("stage", ["read", "match"])
def test_snapshot_wait_preserves_observer_and_matcher_errors(stage):
    error = RuntimeError("observation failed")
    read = Mock(side_effect=error) if stage == "read" else Mock(return_value=["ready"])
    match = Mock(side_effect=error) if stage == "match" else Mock(return_value=True)
    with pytest.raises(RuntimeError) as raised:
        resources.wait_for_snapshot(read, match, "snapshot")
    assert raised.value is error
    assert read.call_count == 1


# 旧调用方在等待命中后又读一遍，会把之后的列表变化误判成过滤失败。
def test_filtered_views_assert_on_the_snapshot_the_wait_matched(monkeypatch):
    world = Mock()
    ui = world.controller.ui
    ui.call.return_value = {"elementHandles": ["queue"]}
    ui.visible.return_value = True
    expected = [
        dislike.CATALOG[key][0] for key in dislike.CATALOG if key not in dislike.BLOCKED["exact"]
    ]
    reads = {None: 0, "queue": 0}

    def rows(ui, scope=None):
        reads[scope] += 1
        return expected if reads[scope] == 1 else ["changed after match"]

    monkeypatch.setattr(dislike, "rows", rows)
    monkeypatch.setattr(dislike, "daily", lambda ui: None)
    monkeypatch.setattr(dislike, "activate", lambda ui, label: None)
    dislike.assert_filtered(world, "exact")
    assert reads == {None: 1, "queue": 1}
