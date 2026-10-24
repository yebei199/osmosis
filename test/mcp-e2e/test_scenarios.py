"""迁移样板的真应用入口；执行时由调用者提供独占应用与数据库。"""

import argparse
import os

import pytest

from pick import run_pick
from views import step_aba, step_enter


@pytest.mark.live
@pytest.mark.parametrize("mode", ["list", "wall"])
def test_pick(mcp, mode):
    """经真实列表或卡墙三连点，断言数据库只起播并发布一次。"""
    run_pick(mcp, mode, os.environ)


@pytest.mark.live
def test_views(mcp, tmp_path):
    """真实 enter 与 ABA 路径采样，禁止任何一帧混入外来曲目。"""
    args = argparse.Namespace(client=mcp, watch=12, port=8091)
    with (tmp_path / "views.jsonl").open("w") as out:
        assert step_enter(args, out)
        assert step_aba(args, out)


def test_pick_rejects_unknown_mode(mcp):
    """错误入口在访问 UI 或数据库之前失败。"""
    with pytest.raises(ValueError, match="mode"):
        run_pick(mcp, "unknown", {})


@pytest.mark.parametrize(
    "plays,publishes,checkpoint,passes",
    [
        (1, 1, "old", True),
        (1, 0, "new", True),
        (1, 0, "old", False),
        (2, 1, "old", False),
        (1, 2, "new", False),
    ],
)
def test_pick_preserves_publication_or_checkpoint_contract(
    monkeypatch,
    plays,
    publishes,
    checkpoint,
    passes,
):
    """只替换 UI/数据库边界，真实场景判据不能接受漏发或重复起播。"""
    import pick

    before = (10, 10, "old", 0)
    after = (10 + plays, 10 + publishes, checkpoint, 0)
    observations = iter([before, after, after, after])
    monkeypatch.setattr(pick, "reset", lambda *args: None)
    monkeypatch.setattr(pick, "show_view", lambda *args: None)
    monkeypatch.setattr(pick, "tap", lambda *args: None)
    monkeypatch.setattr(pick, "snapshot", lambda env: next(observations, after))
    clock = iter(range(0, 1000, 100))
    monkeypatch.setattr(pick.time, "monotonic", lambda: next(clock))
    monkeypatch.setattr(pick.time, "sleep", lambda seconds: None)
    if passes:
        pick.run_pick(None, "list", {})
    else:
        with pytest.raises(AssertionError):
            pick.run_pick(None, "list", {})
