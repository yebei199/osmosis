"""REMOTE-BEHAVIOR-1 的可收集测试，JUnit 名称与验收映射逐一对应。"""

import json
import os
import tempfile
import time
import traceback
from pathlib import Path

import cases
import pytest
from media import DURATION
from resources import World


# 每个 testcase 从新环境开始，失败保留日志和 PCM；初始化失败同样清理。
@pytest.fixture
def world(request):
    artifact_root = Path(os.environ["REMOTE_BEHAVIOR_ARTIFACTS"])
    directory = Path(tempfile.mkdtemp(prefix="175-", dir=artifact_root))
    root = Path(__file__).resolve().parents[2]
    target = Path(os.environ["REMOTE_BEHAVIOR_TARGET_DIR"])
    # 断连心跳最坏90s加重连抖动75s,素材覆盖该前提且不提前播完。
    duration = 240 if request.node.name == "test_last_output_reconnect" else DURATION
    instance = World(
        root, directory, target / "debug/osmosis-desktop", target / "debug/server", duration
    )
    started = time.time()
    try:
        yield instance.start()
    except BaseException:
        (directory / "original-failure.txt").write_text(traceback.format_exc())
        raise
    finally:
        instance.close()
        (directory / "testcase.json").write_text(
            json.dumps(
                {
                    "nodeid": request.node.nodeid,
                    "start": started,
                    "end": time.time(),
                    "candidate": os.environ["REMOTE_BEHAVIOR_COMMIT"],
                },
                indent=2,
            )
        )


# 真实列表与真实卡墙各自贯通，不互相抵扣。
@pytest.mark.parametrize("mode", ("list", "wall"))
def test_core_pick(world, mode):
    cases.remote_pick(world, mode)


# 暂停/继续的判据是采到静音及再次推进的 PCM。
def test_pause_resume(world):
    cases.pause_resume(world)


# 上首/下首必须改变真实输出的曲目编码。
def test_previous_next(world):
    cases.previous_next(world)


# 同一素材的前跳与后跳都必须改变输出位置编码。
def test_seek_both_directions(world):
    cases.seek_both_directions(world)


# 两台输出之间切换必须同时验证新目标播放与旧目标停止。
def test_switch_output(world):
    cases.switch_output(world)


# 建组从本机真实播放接续有效 seed。
def test_local_seed(world):
    cases.local_seed(world)


# 随机与循环必须在遥控和出声两端显示,并影响真实目标音频。
def test_shuffle_and_loop_projection(world):
    cases.shuffle_and_loop_projection(world)


# 独奏 FM 切到远端后必须续上组队列并播放新增曲目。
def test_local_radio_seed_top_up(world):
    cases.local_radio_seed_top_up(world)


# 组内真实私人 FM 在顺序末尾续取,循环和随机都不能阻止 append。
@pytest.mark.parametrize(
    "mode,shuffled",
    [("all", False), ("off", False), ("all", True), ("one", False)],
    ids=["list-loop", "loop-off", "shuffled-loop", "single-loop"],
)
def test_group_radio_sequence_end_top_up(world, mode, shuffled):
    cases.group_radio_sequence_end_top_up(world, mode, shuffled)


# 普通列表在曲尾保持回卷,不得被电台续取机制接管。
def test_non_radio_list_loop_wraps_without_append(world):
    cases.non_radio_list_loop_wraps_without_append(world)


# 健康组不能被无关本机 seed 覆盖。
def test_healthy_group_ignores_unrelated_seed(world):
    cases.healthy_group_ignores_unrelated_seed(world)


# 空组选择输出不自启，随后点歌仍可用。
def test_idle_group(world):
    cases.idle_group(world)


RECOVER_AXES = {
    "damage": ("queue", "revision", "entry"),
    "order": ("direct", "recovered"),
    "seed": ("playing", "empty"),
}
# #185 由 3x2x2 全组合缩为 4 条：每种失效至少一条，order 与 seed 两两组合全覆盖。
# 三种失效只在 install_invalid 写入的列上不同，恢复路径都是服务端把整组引用清空，
# 之后的分支只由 order 与 seed 决定，所以删掉的 8 条只是重复走同一恢复分支。
# revision-direct-playing 是 mutate.py recover-reference 故障指向的用例，必须保留。
RECOVER_CASES = (
    ("revision", "direct", "playing"),
    ("queue", "recovered", "playing"),
    ("entry", "direct", "empty"),
    ("revision", "recovered", "empty"),
)


# 删减后的矩阵仍让每个取值至少出现一次。
def test_recover_invalid_covers_every_value():
    for index, (axis, values) in enumerate(RECOVER_AXES.items()):
        assert {case[index] for case in RECOVER_CASES} == set(values), axis
    pairs = {(order, seed) for _, order, seed in RECOVER_CASES}
    assert pairs == {(o, s) for o in RECOVER_AXES["order"] for s in RECOVER_AXES["seed"]}


# 三种失效、两种进入顺序和有/无 seed 都覆盖恢复后的遥控旧功能。
@pytest.mark.parametrize(
    ("damage", "order", "seed"),
    [pytest.param(*case, id="-".join(case)) for case in RECOVER_CASES],
)
def test_recover_invalid(world, damage, order, seed):
    cases.recover_invalid(world, damage, order, seed)


# 电台多次发布后仍能从暂停组继续原引用并再次遥控。
def test_radio_publication_preserves_group_reference(world):
    cases.radio_publication_preserves_group_reference(world)


# 真实离组后跨轮询刷新保留入口，点击之前静音，之后本机PCM和新checkpoint。
@pytest.mark.parametrize("origin", ("fm", "list"))
def test_retained_local_playback_entry(world, origin):
    cases.retained_local_playback_entry(world, origin)


# 真正空的本机队列不得残留远端曲目的播放入口或自动起播。
def test_empty_local_queue_stays_empty(world):
    cases.empty_local_queue_stays_empty(world)


# 非出声成员的曲目/控制依然来自组投影，不能被保留的本机曲目覆盖。
def test_silent_member_projects_group_track(world):
    cases.silent_member_projects_group_track(world)


# 唯一目标断线、重连、手动继续由实际音频验证。
def test_last_output_reconnect(world):
    cases.last_output_reconnect(world)


# 多输出时一台断线不打断另一台，重连按最新状态执行。
def test_one_of_two_outputs_reconnect(world):
    cases.one_of_two_outputs_reconnect(world)


# 服务重启后版本和可控制性必须保持。
def test_server_restart(world):
    cases.server_restart(world)


# 每个变异独立编译固定快照，编译/启动错误不会被计作有效 RED。
@pytest.mark.parametrize(
    "fault", ("send-request", "execute-target", "recover-reference", "protect-reference")
)
def test_fault_sensitivity(fault):
    from mutate import verify

    root = Path(__file__).resolve().parents[2]
    verify(root, Path(os.environ["REMOTE_BEHAVIOR_ARTIFACTS"]), fault)
