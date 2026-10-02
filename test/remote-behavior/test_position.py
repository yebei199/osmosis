"""F-001 的判例：业务库与播放器一起错误移动也不能通过独立音频边界。"""

import pytest

from position import Action, Observation, check, continuation, resumed


# 旧末帧位于媒体 20 秒且单调时钟约 100 秒；构造数据只验证判据，不抵扣真实 PCM。
def prior():
    return Observation((19,) * 5 + (20,) * 5, 99.0, 100.25)


# 正确的非零接续符合原容差。
def test_handoff_accepts_continuation():
    before = prior()
    after = Observation((23,) * 10, 102.0, 103.25)
    check(before, after, continuation(before, after))


# 错误回零不能由操作后 DB 同样回零而被豁免。
def test_handoff_rejects_reset():
    before = prior()
    after = Observation((0,) * 10, 102.0, 103.25)
    with pytest.raises(AssertionError, match="independent continuation"):
        check(before, after, continuation(before, after))


# DB 与声音同时异常前跳八秒，仍超出操作前推导范围。
def test_handoff_rejects_forward_jump():
    before = prior()
    after = Observation((31,) * 10, 102.0, 103.25)
    with pytest.raises(AssertionError, match="independent continuation"):
        check(before, after, continuation(before, after))


# 暂停十秒后继续，正确行为停在原播放点附近。
def test_pause_accepts_frozen_position():
    before = prior()
    after = Observation((21,) * 10, 111.5, 112.75)
    bounds = resumed(before, after, Action(100.3, 100.4), 100.8, Action(111.0, 111.1))
    check(before, after, bounds)


# 静音期间继续计时十秒，会落到独立暂停范围之外。
def test_pause_rejects_elapsed_time():
    before = prior()
    after = Observation((31,) * 10, 111.5, 112.75)
    bounds = resumed(before, after, Action(100.3, 100.4), 100.8, Action(111.0, 111.1))
    with pytest.raises(AssertionError, match="independent continuation"):
        check(before, after, bounds)
