"""仅从操作前 PCM 与单调时钟推导接续范围，不读操作后的业务状态。"""

import time
from dataclasses import asdict, dataclass

from media import FRAME_SECONDS, POSITION_TOLERANCE

MIN_POSITION = 12
MIN_ADVANCE = 2
PAUSE_HOLD = 6


# 秒码表示一个媒体秒区间，时间取被识别帧的中点。
@dataclass(frozen=True)
class Observation:
    seconds: tuple[int, ...]
    started: float
    ended: float

    @property
    def first_time(self):
        return self.started + FRAME_SECONDS / 2

    @property
    def last_time(self):
        return self.started + (len(self.seconds) - 0.5) * FRAME_SECONDS

    def __getitem__(self, index):
        return self.seconds[index]


# 一次真实 UI 动作的发出与返回界限，不假定返回意味着目标已经执行。
@dataclass(frozen=True)
class Action:
    started: float
    ended: float


# 包住现有 UI 入口，返回本轮单调时钟原始时间。
def act(callback):
    started = time.monotonic()
    callback()
    return Action(started, time.monotonic())


# 连续播放接续：新的首帧只能落在旧末帧媒体区间加实际经过时间内。
def continuation(before, after):
    elapsed = after.first_time - before.last_time
    if elapsed < 0:
        raise ValueError("PCM observations are not ordered")
    return before[-1] + elapsed, before[-1] + 1 + elapsed


# 暂停生效在请求发出与首个静音窗口之间；继续生效在请求发出与新首帧之间。
def resumed(before, after, pause, silent_started, resume):
    if (
        not before.last_time
        <= pause.started
        <= silent_started
        <= resume.started
        <= after.first_time
    ):
        raise ValueError("pause/resume evidence is not ordered")
    earliest = before[-1] + pause.started - before.last_time
    latest = before[-1] + 1 + silent_started - before.last_time + after.first_time - resume.started
    return earliest, latest


# 容差只在最终 PCM 比较加一次；失败文书保留未放宽的上下界与原始样本。
def check(before, after, bounds, action=None):
    if (
        action is not None
        and not before.last_time <= action.started <= action.ended <= after.first_time
    ):
        raise ValueError("UI action and PCM observations are not ordered")
    low, high = bounds
    record = {
        "before": asdict(before),
        "after": asdict(after),
        "bounds": [low, high],
        "tolerance": POSITION_TOLERANCE,
        "action": None if action is None else asdict(action),
    }
    if not low - POSITION_TOLERANCE <= after[0] <= high + POSITION_TOLERANCE:
        raise AssertionError(f"audio: independent continuation failed: {record}")
    return record
