"""遥控旧功能与生命周期的用户路径，最终判断使用实际 PCM。"""

import json
import time
from pathlib import Path

from media import DAILY_IDS
from position import act
from resources import AudioFailure, wait_until


# 首条纵向用例从空组选择目标，再从真实列表/卡墙起播。
def remote_pick(world, mode="list"):
    source = world.controller
    source.ui.output(world.one)
    world.silent(*world.clients)
    source.ui.pick(mode, index=0 if mode == "list" else 1)
    wanted = DAILY_IDS[0 if mode == "list" else 1]
    result = world.sound(world.one, wanted)
    world.silent(source, world.two)
    row = world.group()
    source.ui.no_error_banner()
    assert row["outputs"] == [world.one.device()]
    assert source.device() in row["members"]
    assert row["queue_id"] is not None
    return result


# 暂停后必须静音，继续后 PCM 秒码必须从暂停点向前推进。
def pause_resume(world):
    remote_pick(world)
    before = world.progressed(world.one, DAILY_IDS[0])
    source = world.controller
    pause = act(lambda: source.ui.transport("pause"))
    quiet = world.silent(world.one)[0]
    world.silent(source, world.two)
    world.held_silent(world.one)
    stopped = world.group()
    assert not stopped["playing"]
    resume = act(lambda: source.ui.transport("resume"))
    after = world.sound(world.one, DAILY_IDS[0])
    world.continued(before, after, resume, (pause, quiet["monotonic_start"], resume))
    world.silent(source, world.two)
    later = world.sound(world.one, DAILY_IDS[0])
    if later[-1] <= after[0]:
        raise AudioFailure(f"audio: resumed media did not advance: {after} -> {later}")


# 上下首分别核对不同素材，防止只改变曲名或重复播放旧音频。
def previous_next(world):
    remote_pick(world)
    source = world.controller
    source.ui.transport("next")
    world.sound(world.one, DAILY_IDS[1])
    source.ui.transport("prev")
    world.sound(world.one, DAILY_IDS[0])
    world.silent(source, world.two)


# 前跳和后跳都通过 ProgressBar 拖动，并从音频秒码核对落点。
def seek_both_directions(world):
    remote_pick(world)
    source = world.controller
    source.ui.transport("pause")
    world.silent(*world.clients)
    source.ui.seek(40)
    world.silent(*world.clients)
    resumed = time.time()
    source.ui.transport("resume")
    world.sound(world.one, DAILY_IDS[0], position=40, position_started=resumed)
    source.ui.transport("pause")
    world.silent(*world.clients)
    source.ui.seek(8)
    world.silent(*world.clients)
    resumed = time.time()
    source.ui.transport("resume")
    world.sound(world.one, DAILY_IDS[0], position=8, position_started=resumed)
    world.silent(source, world.two)


# 切换输出后新目标实际出声，旧目标和遥控器均停止。
def switch_output(world):
    remote_pick(world)
    before = world.progressed(world.one, DAILY_IDS[0])
    action = act(lambda: world.controller.ui.output(world.two))
    after = world.sound(world.two, DAILY_IDS[0])
    world.continued(before, after, action)
    world.silent(world.one, world.controller)
    assert world.group()["outputs"] == [world.two.device()]


# 本机已经播放时建组，应接续本机 seed 并停止源设备输出。
def local_seed(world):
    source = world.controller
    source.ui.pick("list")
    before = world.progressed(source, DAILY_IDS[0], group=False)
    action = act(lambda: source.ui.output(world.one))
    after = world.sound(world.one, DAILY_IDS[0])
    world.continued(before, after, action)
    world.silent(source, world.two)


# 加入健康组时，源端的另一首本机曲目不能覆盖已有组播放。
def healthy_group_ignores_unrelated_seed(world):
    remote_pick(world)
    source = world.controller
    source.ui.leave()
    wait_until(lambda: source.device() not in world.group()["members"], "leave group")
    source.ui.pick("list", 2)
    world.sound(source, DAILY_IDS[2], group=False)
    source.ui.output(world.one)
    world.sound(world.one, DAILY_IDS[0])
    world.silent(source, world.two)
    row = world.group()
    entry = world.sql(
        "SELECT track_id FROM play_queue_entries WHERE queue_id=%s AND revision=%s AND entry_id=%s",
        (row["queue_id"], row["revision"], row["entry_id"]),
    )
    assert entry[0]["track_id"] == DAILY_IDS[0]


# 空组选择输出仍空闲，下一次真实点歌才能产生声音。
def idle_group(world):
    source = world.controller
    source.ui.output(world.one)
    world.silent(*world.clients)
    row = world.group()
    assert not row["playing"]
    assert row["queue_id"] is None and row["revision"] is None and row["entry_id"] is None
    source.ui.pick("list", 2)
    world.sound(world.one, DAILY_IDS[2])
    world.silent(source, world.two)


# 数据库只造失效历史引用，源客户端的有效本机 seed 和播放器保留真实状态。
def install_invalid(world, damage):
    source = world.controller
    queues = world.sql(
        "SELECT id, revision FROM play_queues WHERE account_id=%s AND device_id=%s ORDER BY id DESC",
        (world.account, source.device()),
    )
    if queues:
        queue = queues[0]
        entry = world.sql(
            "SELECT entry_id FROM play_queue_entries WHERE queue_id=%s AND revision=%s ORDER BY position LIMIT 1",
            (queue["id"], queue["revision"]),
        )[0]["entry_id"]
    else:
        queue = world.sql(
            "INSERT INTO play_queues(account_id,device_id,revision,next_entry_id) VALUES(%s,%s,1,2) RETURNING id,revision",
            (world.account, "fixture-history"),
        )[0]
        entry = 1
        world.sql(
            """INSERT INTO play_queue_entries
               (queue_id,revision,entry_id,position,platform,track_id,title,artists,duration_ms)
               VALUES(%s,1,1,0,'netease','175001','RB-history',ARRAY['RemoteBehavior'],90000)""",
            (queue["id"],),
        )
    reference = queue["id"]
    revision = queue["revision"] + (1000000 if damage == "revision" else 0)
    if damage == "entry":
        entry += 1000000
    if damage == "queue":
        # 已删除的队列通过 FK 留下部分历史引用；独立空队列不伤本机 seed。
        reference = world.sql(
            "INSERT INTO play_queues(account_id,device_id,revision,next_entry_id) VALUES(%s,%s,1,1) RETURNING id",
            (world.account, "fixture-deleted"),
        )[0]["id"]
    world.sql(
        """INSERT INTO play_groups(account_id,version,members,outputs,queue_id,revision,entry_id,playing)
        VALUES(%s,1,%s,%s,%s,%s,%s,false)
        ON CONFLICT(account_id) DO UPDATE SET version=play_groups.version+1,
        members=EXCLUDED.members,outputs=EXCLUDED.outputs,queue_id=EXCLUDED.queue_id,
        revision=EXCLUDED.revision,entry_id=EXCLUDED.entry_id,playing=false,
        position_us=0,anchor_wall_us=0,boundary_wall_us=NULL,alive_wall_us=NULL,play_order='{}'""",
        (world.account, [world.one.device()], [world.one.device()], reference, revision, entry),
    )
    if damage == "queue":
        world.sql("DELETE FROM play_queues WHERE id=%s", (reference,))
    (world.directory / "corruption.json").write_text(
        json.dumps(
            {
                "damage": damage,
                "group": world.group(),
                "time": time.time(),
            },
            default=str,
            indent=2,
        )
    )


# 先收到恢复空组再切输出，以及直接切输出触发恢复，使用相同最终用户路径。
def recover_invalid(world, damage, order, seed):
    source = world.controller
    wanted = DAILY_IDS[0]
    before = None
    if seed == "playing":
        source.ui.radio()
        # 真实取到的第一批 FM 标识由输入目录固定，目标不能由夹具回报播放成功。
        wanted = "175004"
        before = world.progressed(source, wanted, group=False)
        wait_until(
            lambda: world.sql(
                "SELECT 1 FROM play_queue_reports r JOIN play_queues q ON q.id=r.queue_id WHERE q.account_id=%s AND q.device_id=%s",
                (world.account, source.device()),
            ),
            "local queue checkpoint",
        )
    install_invalid(world, damage)
    if order == "recovered":
        source.restart()
        wait_until(
            lambda: (
                world.group()["queue_id"] is None
                and world.group()["revision"] is None
                and world.group()["entry_id"] is None
            ),
            "persisted recovery",
        )
        # 服务端清空已经落库，还须证明对应版本到达真实客户端。
        version = world.group()["version"]
        marker = f"\u7ec4\u72b6\u6001: \u7b2c {version} \u7248,"
        wait_until(
            lambda: marker in Path(source.process.log.name).read_text(), "recovered client state"
        )
        if seed == "playing":
            source.ui.pick("list")
            wanted = DAILY_IDS[0]
            before = world.progressed(source, wanted, group=False)
    action = act(lambda: source.ui.output(world.one))
    if seed == "playing":
        # 先看最终音频，404/意图未执行会停在此处，构成有效行为 RED。
        after = world.sound(world.one, wanted)
        assert before is not None
        world.continued(before, after, action)
        world.silent(source, world.two)
    else:
        world.silent(*world.clients)
        row = world.group()
        assert row["queue_id"] is None and not row["playing"]
    assert world.one.device() in world.group()["members"]
    source.ui.pick("list", 1)
    world.sound(world.one, DAILY_IDS[1])
    source.ui.transport("pause")
    world.silent(*world.clients)
    source.ui.transport("resume")
    world.sound(world.one, DAILY_IDS[1])
    source.ui.output(world.two)
    world.sound(world.two, DAILY_IDS[1])
    world.silent(source, world.one)
    source.ui.leave()
    wait_until(lambda: source.device() not in world.group()["members"], "recovered group leave")
    world.silent(source)
    source.ui.pick("list")
    world.sound(source, DAILY_IDS[0], group=False)
    world.sound(world.two, DAILY_IDS[1])
    source.ui.no_error_banner()


# 本轮动作之后的真实本机上报；旧 playing report 不能建立续取前提。
def local_radio_report(world, source, queue_id, after):
    return world.sql(
        """SELECT q.id,r.applied_revision AS revision,r.entry_id,e.track_id,r.reported_at
        FROM play_queues q JOIN play_queue_reports r ON q.id=r.queue_id
        JOIN play_queue_entries e ON e.queue_id=q.id AND e.revision=r.applied_revision
          AND e.entry_id=r.entry_id
        WHERE q.account_id=%s AND q.device_id=%s AND q.id=%s
          AND r.play_state='playing' AND r.reported_at>%s
        ORDER BY r.reported_at DESC LIMIT 1""",
        (world.account, source.device(), queue_id, after),
    )


# 已有真实本机曲目与远端曲目不同；离组后的入口必须回到本地保留曲目。
def retained_local_playback_entry(world, origin):
    source = world.controller
    wanted = "175004" if origin == "fm" else DAILY_IDS[2]
    if origin == "fm":
        source.ui.radio()
    else:
        source.ui.pick("list", 2)
    world.sound(source, wanted, group=False)
    queue = world.sql(
        "SELECT id FROM play_queues WHERE account_id=%s AND device_id=%s ORDER BY id DESC LIMIT 1",
        (world.account, source.device()),
    )[0]["id"]
    source.ui.output(world.one)
    world.sound(world.one, wanted)
    world.one.ui.pick("list", 0)
    world.sound(world.one, DAILY_IDS[0])
    source.ui.playback_entry(DAILY_IDS[0], True)
    world.one.ui.transport("pause")
    world.silent(*world.clients)
    prior = world.group().copy()
    source.ui.leave()
    wait_until(lambda: source.device() not in world.group()["members"], "retained source left")
    source.ui.playback_entry(wanted, False)
    world.held_silent(source)
    source.ui.playback_entry(wanted, False)
    world.silent(world.one, world.two)
    after = world.sql("SELECT clock_timestamp() AS started")[0]["started"]
    source.ui.transport("resume")
    world.sound(source, wanted, group=False)
    reports = wait_until(
        lambda: local_radio_report(world, source, queue, after), "local replay checkpoint"
    )
    assert reports[0]["track_id"] == wanted
    row = world.group()
    assert not row["playing"] and source.device() not in row["members"]
    for key in ("queue_id", "revision", "entry_id"):
        assert row[key] == prior[key], f"local replay changed group {key}"
    world.silent(world.one, world.two)
    source.ui.playback_entry(wanted, True)
    (world.directory / "local-replay-checkpoint.json").write_text(
        json.dumps({"prior": prior, "after": row, "checkpoint": reports[0]}, default=str, indent=2)
    )


# 无本地曲目的遥控器离组后仍为空，不把远端曲目当成本机可重播曲目。
def empty_local_queue_stays_empty(world):
    remote_pick(world)
    source = world.controller
    source.ui.playback_entry(DAILY_IDS[0], True)
    source.ui.transport("pause")
    world.silent(*world.clients)
    prior = world.group().copy()
    source.ui.leave()
    wait_until(lambda: source.device() not in world.group()["members"], "empty source left")
    world.held_silent(source)
    source.ui.no_playback_entry()
    world.silent(world.one, world.two)
    row = world.group()
    assert not row["playing"]
    for key in ("queue_id", "revision", "entry_id"):
        assert row[key] == prior[key]


# 保留本地曲目的非出声成员跨刷新仍显示组曲目，暂停/继续走组而非本机。
def silent_member_projects_group_track(world):
    source = world.controller
    source.ui.pick("list", 2)
    world.sound(source, DAILY_IDS[2], group=False)
    source.ui.output(world.one)
    world.one.ui.pick("list", 0)
    world.sound(world.one, DAILY_IDS[0])
    source.ui.playback_entry(DAILY_IDS[0], True)
    world.held_silent(source)
    source.ui.playback_entry(DAILY_IDS[0], True)
    source.ui.transport("pause")
    world.silent(*world.clients)
    source.ui.playback_entry(DAILY_IDS[0], False)
    world.held_silent(source)
    source.ui.playback_entry(DAILY_IDS[0], False)
    source.ui.transport("resume")
    world.sound(world.one, DAILY_IDS[0])
    world.silent(source, world.two)
    assert source.device() in world.group()["members"]
    assert world.group()["outputs"] == [world.one.device()]


# 明确点击真实继续建立本机续取前提，暂停组及原引用必须保持。
def continue_local_radio(world, source, prior):
    wait_until(lambda: source.device() not in world.group()["members"], "FM source left group")
    source.ui.radio()
    started = world.sql("SELECT clock_timestamp() AS started")[0]["started"]
    source.ui.transport("resume")
    world.sound(source, "175004", group=False)
    row = world.group()
    assert not prior["playing"] and not row["playing"]
    assert source.device() not in row["members"]
    for key in ("queue_id", "revision", "entry_id"):
        assert row[key] == prior[key], f"local FM resume changed group {key}"
    (world.directory / "local-fm-premise.json").write_text(
        json.dumps({"after": started, "prior": prior, "after_resume": row}, default=str, indent=2)
    )
    return started


# 多次电台发布超过保留窗后，暂停组仍能继续原引用音频。
def radio_publication_preserves_group_reference(world):
    source = world.controller
    source.ui.radio()
    world.sound(source, "175004", group=False)
    source.ui.output(world.one)
    world.sound(world.one, "175004")
    world.one.ui.transport("pause")
    world.silent(*world.clients)
    prior = world.group().copy()
    source.ui.leave()
    after = continue_local_radio(world, source, prior)
    for _ in range(12):
        reports = wait_until(
            lambda after=after: local_radio_report(world, source, prior["queue_id"], after),
            "local FM progress",
        )
        with (world.directory / "local-fm-publications.jsonl").open("a") as log:
            log.write(json.dumps(reports[0], default=str) + "\n")
        world.sound(source, reports[0]["track_id"], group=False)
        if (
            reports[0]["id"] == prior["queue_id"]
            and reports[0]["revision"] >= prior["revision"] + 4
        ):
            break
        after = world.sql("SELECT clock_timestamp() AS started")[0]["started"]
        source.ui.transport("next")
    else:
        raise RuntimeError(
            "fixture never exercised four newer publications of the group-referenced queue"
        )
    # 恢复意图经过真实 server，回收保护故障必须在最终音频断言上失败。
    world.one.ui.transport("resume")
    world.sound(world.one, "175004")
    world.silent(world.two)
    count = world.sql(
        "SELECT count(*) AS n FROM play_queue_entries WHERE queue_id=%s AND revision=%s AND entry_id=%s",
        (prior["queue_id"], prior["revision"], prior["entry_id"]),
    )[0]["n"]
    assert count == 1
    row = world.group()
    assert source.device() not in row["members"]
    assert row["outputs"] == [world.one.device()]
    # 独奏者点健康组的另一输出先追加，原输出必须继续播放。
    source.ui.output(world.two)
    world.sound(world.two, "175004")
    world.sound(world.one, "175004")
    world.silent(source)
    row = world.group()
    assert source.device() in row["members"]
    assert set(row["outputs"]) == {world.one.device(), world.two.device()}
    # 加入已由真实状态确认，成员再次点目标才是替换。
    source.ui.output(world.two)
    world.sound(world.two, "175004")
    world.silent(world.one, source)
    assert world.group()["outputs"] == [world.two.device()]


# 唯一输出掉线后服务端暂停；恢复连接仍暂停，UI 继续后恢复音频。
def last_output_reconnect(world):
    remote_pick(world)
    world.one.gate.cut()
    world.silent(*world.clients)
    # 闸保留服务端半连接,等待生产 30s×两次容忍的最坏相位清退。
    wait_until(
        lambda: not world.group()["playing"],
        "last output removal pauses group",
        timeout=100,
    )
    world.one.gate.heal()
    wait_until(
        lambda: world.one.ui.find("RoundControl::touch", "\u64ad\u653e"),
        "reconnected paused target",
    )
    world.silent(*world.clients)
    world.controller.ui.transport("resume")
    world.sound(world.one, DAILY_IDS[0])
    world.silent(world.controller, world.two)


# 一台目标掉线时另一台继续；重连目标必须按最新时间线出声。
def one_of_two_outputs_reconnect(world):
    remote_pick(world)
    source = world.controller
    source.ui.join_output(world.two)
    world.sound(world.two, DAILY_IDS[0])
    world.one.gate.cut()
    world.silent(world.one, source)
    world.sound(world.two, DAILY_IDS[0])
    assert world.group()["playing"]
    world.one.gate.heal()
    world.sound(world.one, DAILY_IDS[0])
    world.silent(source)
    source.ui.transport("pause")
    world.silent(*world.clients)
    source.ui.transport("resume")
    world.sound(world.one, DAILY_IDS[0])
    world.sound(world.two, DAILY_IDS[0])


# 服务重启保持版本不回退；恢复后仍暂停，真实 UI 操作重新出声。
def server_restart(world):
    remote_pick(world)
    prior = world.group().copy()
    world.restart_server()
    world.silent(*world.clients)
    wait_until(lambda: not world.group()["playing"], "server restart pause")
    assert world.group()["version"] >= prior["version"]
    world.controller.ui.transport("resume")
    world.sound(world.one, DAILY_IDS[0])
    world.silent(world.controller, world.two)
