//! `timeline.rs` 的测试:全局播放状态的纯规则。

use contract::LoopModeDto;

use super::timeline::*;

const SEC: i64 = 1_000_000;
const TRACK: u64 = 100 * SEC as u64;

/// 三首,各 100 秒。
fn list() -> Playlist {
    Playlist {
        entries: vec![
            (1, TRACK, false),
            (2, TRACK, false),
            (3, TRACK, false),
        ],
    }
}

/// `list()` 的三首里,把 `entry_id` 那一条标成命中屏蔽规则。
fn blocking(entry_id: i64) -> Playlist {
    let mut list = list();
    for entry in &mut list.entries {
        if entry.0 == entry_id {
            entry.2 = true;
        }
    }
    list
}

/// pc 在放第 1 条,手机只当遥控器。
fn group() -> Group {
    let mut group = Group {
        version: 1,
        ..Group::default()
    };
    group.set_outputs("phone", vec!["pc".to_owned()]);
    group
        .jump("phone", (7, 1), &list(), 1, 0, 42)
        .expect("成员点歌该成");
    group
}

fn now(group: &Group) -> &Now {
    group.now.as_ref().expect("该有在放的")
}

/// 发意图的一台与出声设备都成为成员,出声设备是成员的子集。
#[test]
fn choosing_outputs_makes_everyone_involved_a_member() {
    let group = group();

    assert_eq!(group.members, vec!["phone", "pc"]);
    assert_eq!(group.outputs, vec!["pc"]);
}

/// 组外设备的意图一律被拒:组外点歌走本机。
#[test]
fn a_non_member_cannot_steer_the_group() {
    let mut group = group();

    assert_eq!(
        group.pause("stranger", &list(), SEC),
        Err(Refusal::NotMember)
    );
}

/// 点歌先锚在「现在 + START_WAIT」:等出声设备取流(#154),到点谁没好也照常开播。
#[test]
fn a_jump_starts_together_a_moment_from_now() {
    let group = group();

    assert_eq!(now(&group).anchor_wall_us, START_WAIT_US);
    assert_eq!(
        now(&group).position_at(START_WAIT_US - 1, TRACK),
        0,
        "没人报就绪,到上限之前一直停在开头"
    );
    assert_eq!(
        now(&group)
            .position_at(START_WAIT_US + 2 * SEC, TRACK),
        2 * SEC as u64
    );
}

/// 暂停停在那一刻的位置,继续从那里接着放。
#[test]
fn pause_and_resume_keep_the_position() {
    let mut group = group();

    group
        .pause("pc", &list(), START_WAIT_US + 10 * SEC)
        .unwrap();
    assert!(!now(&group).playing);
    assert_eq!(now(&group).position_us, 10 * SEC as u64);

    group.resume("phone", &list(), 60 * SEC).unwrap();
    assert_eq!(
        now(&group).anchor_wall_us,
        60 * SEC + LEAD_US
    );
    assert_eq!(now(&group).position_us, 10 * SEC as u64);
}

/// 下一首照次序;不循环时队尾的下一首不动,上一首放过三秒就回到开头。
#[test]
fn next_and_prev_follow_the_order() {
    let mut group = group();
    group.step("phone", &list(), 1, SEC).unwrap();
    assert_eq!(now(&group).entry_id, 2);

    group.step("phone", &list(), 1, 2 * SEC).unwrap();
    group.step("phone", &list(), 1, 3 * SEC).unwrap();
    assert_eq!(
        now(&group).entry_id,
        3,
        "队尾不循环就停在这里"
    );

    group
        .step(
            "phone",
            &list(),
            -1,
            3 * SEC + START_WAIT_US + SEC,
        )
        .unwrap();
    assert_eq!(
        now(&group).entry_id,
        2,
        "刚放一秒,上一首就是前一首"
    );

    let later = 3 * SEC + 2 * START_WAIT_US + 10 * SEC;
    group.step("phone", &list(), -1, later).unwrap();
    assert_eq!(
        now(&group).entry_id,
        2,
        "放过三秒就回到开头"
    );
    assert_eq!(
        now(&group).anchor_wall_us,
        later + START_WAIT_US
    );
}

/// 列表循环时队尾接回队头。
#[test]
fn looping_all_wraps_around() {
    let mut group = group();
    group.set_loop("pc", LoopModeDto::All).unwrap();
    group.jump("pc", (7, 1), &list(), 3, 0, 1).unwrap();

    group.step("pc", &list(), 1, SEC).unwrap();

    assert_eq!(now(&group).entry_id, 1);
}

/// 下一首命中屏蔽规则就再跳一首;上一首同理(#167)。
#[test]
fn next_and_prev_skip_a_blocked_entry() {
    let mut group = group();
    group.step("phone", &blocking(2), 1, SEC).unwrap();
    assert_eq!(
        now(&group).entry_id,
        3,
        "2 被屏蔽,下一首该是 3"
    );

    group
        .step(
            "phone",
            &blocking(2),
            -1,
            SEC + START_WAIT_US + SEC,
        )
        .unwrap();
    assert_eq!(
        now(&group).entry_id,
        1,
        "上一首同样跳过被屏蔽的 2"
    );
}

/// 自然放完(`roll`/`advance` 走的都是 `follower`)同样跳过被屏蔽的下一首。
#[test]
fn a_natural_finish_skips_a_blocked_entry() {
    let mut group = group();
    let list = blocking(2);
    let version = group.version;
    group
        .advance(
            "pc",
            &list,
            1,
            version,
            START_WAIT_US + 100 * SEC,
        )
        .unwrap();
    assert_eq!(now(&group).entry_id, 3);
}

/// 除了在放这一首,其余全被屏蔽:停在这一首末尾,与放到队尾一致。
#[test]
fn everything_else_blocked_stops_at_the_tail() {
    let mut group = group();
    let mut list = list();
    for entry in &mut list.entries {
        if entry.0 != 1 {
            entry.2 = true;
        }
    }
    group.step("phone", &list, 1, SEC).unwrap();
    assert_eq!(
        now(&group).entry_id,
        1,
        "没有可跳的下一首,不动"
    );
}

/// 单曲循环重放的是这一首自己,即便它命中了屏蔽规则也不跳(#167)。
#[test]
fn looping_one_does_not_skip_the_blocked_current_track() {
    let mut group = group_with_loop(LoopModeDto::One);
    group.roll(
        &blocking(1),
        START_WAIT_US + 100 * SEC + ADVANCE_GRACE_US,
    );
    assert_eq!(now(&group).entry_id, 1);
    assert!(now(&group).playing);
}

/// 最先真正放完的出声设备报上来就推进,下一首稍后一起开始;同一份报告第二次到(另一台
/// 也放完了、或者重发)因为版本已经变了而作废(AC-9)。
#[test]
fn the_first_output_to_finish_advances_once() {
    let mut group = group();
    let version = group.version;
    let end = START_WAIT_US + 100 * SEC;

    assert_eq!(
        group.advance("pc", &list(), 1, version, end),
        Ok(true)
    );
    assert_eq!(now(&group).entry_id, 2);
    assert_eq!(
        now(&group).anchor_wall_us,
        end + START_WAIT_US
    );

    group.version += 1;
    assert_eq!(
        group.advance("pc", &list(), 1, version, end + 1),
        Ok(false),
        "迟到的同一份报告不再推进"
    );
    assert_eq!(now(&group).entry_id, 2);
}

/// 报的不是此刻那一条(别处已经切了歌),什么都不动;只当遥控器的不算数。
#[test]
fn a_stale_or_silent_report_does_not_advance() {
    let mut group = group();
    let version = group.version;

    assert_eq!(
        group.advance("pc", &list(), 2, version, SEC),
        Ok(false)
    );
    assert_eq!(
        group.advance("phone", &list(), 1, version, SEC),
        Err(Refusal::NotMember)
    );
    assert_eq!(now(&group).entry_id, 1);
}

/// 兜底:元数据时长到了不推(不截歌尾),再过宽限还没人报放完才推。
#[test]
fn the_server_only_rolls_after_the_grace() {
    let mut group = group();
    let end = START_WAIT_US + 100 * SEC;

    assert!(!group.roll(&list(), end));
    assert!(
        !group.roll(&list(), end + ADVANCE_GRACE_US - 1)
    );
    assert!(group.roll(&list(), end + ADVANCE_GRACE_US));

    assert_eq!(now(&group).entry_id, 2);
    assert_eq!(
        now(&group).anchor_wall_us,
        end + ADVANCE_GRACE_US + START_WAIT_US
    );
}

/// 不循环放到队尾就停在最后一首的末尾;停在队尾后继续从这一首开头放;单曲循环一直放同一首。
#[test]
fn advancing_stops_at_the_tail_or_repeats_one() {
    let mut group = group();
    group.jump("pc", (7, 1), &list(), 3, 0, 1).unwrap();
    let version = group.version;
    group
        .advance(
            "pc",
            &list(),
            3,
            version,
            START_WAIT_US + 100 * SEC,
        )
        .unwrap();
    assert!(!now(&group).playing);
    assert_eq!(now(&group).position_us, TRACK);
    group.resume("pc", &list(), 200 * SEC).unwrap();
    assert_eq!(now(&group).position_us, 0);

    let mut group = group_with_loop(LoopModeDto::One);
    group.roll(
        &list(),
        START_WAIT_US + 100 * SEC + ADVANCE_GRACE_US,
    );
    assert_eq!(now(&group).entry_id, 1);
    assert!(now(&group).playing);
}

fn group_with_loop(mode: LoopModeDto) -> Group {
    let mut group = group();
    group.set_loop("pc", mode).unwrap();
    group
}

/// 最后一台出声设备掉线:立刻暂停,位置记在那一刻。只当遥控器的掉线不影响。
#[test]
fn the_last_output_going_offline_pauses_the_group() {
    let mut group = group();

    assert!(
        !group.pause_if_silent(
            &list(),
            |id| id == "pc",
            10 * SEC
        ),
        "遥控器掉线,出声设备还在"
    );
    assert!(group.pause_if_silent(
        &list(),
        |_| false,
        START_WAIT_US + 10 * SEC
    ));
    assert!(!now(&group).playing);
    assert_eq!(now(&group).position_us, 10 * SEC as u64);
}

/// 出声设备退出组:它离开成员与出声设备;最后一个成员也走了,组就散了。
#[test]
fn leaving_drops_the_device_and_the_last_one_dissolves_the_group()
 {
    let mut group = group();

    group.leave("pc");
    assert_eq!(group.outputs, Vec::<String>::new());
    assert_eq!(group.members, vec!["phone"]);

    group.leave("phone");
    assert!(group.is_vacant());
    assert_eq!(group.now, None);
}

/// 随机从当前这一首起重洗,其余每一条都在、不重复;关上回到原序。
#[test]
fn shuffling_keeps_the_current_track_first_and_everything_once()
 {
    let mut group = group();

    group.shuffle("pc", &list(), true, 12345).unwrap();

    let order = &now(&group).order;
    assert_eq!(order[0], 1);
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![1, 2, 3]);

    group.shuffle("pc", &list(), false, 0).unwrap();
    assert!(now(&group).order.is_empty());
}

/// 点的那一条不在这一版里:拒掉,不瞎放。
#[test]
fn jumping_to_a_missing_entry_is_refused() {
    let mut group = group();

    assert_eq!(
        group.jump("pc", (7, 1), &list(), 99, 0, 1),
        Err(Refusal::NoSuchEntry)
    );
}

/// 电台续歌(#165):组队列换到续上的那一版,在放的这一条与时间线不动,
/// 随机时新条目排在次序末尾。
#[test]
fn extending_moves_to_the_new_revision_and_leaves_the_timeline()
 {
    let mut group = group();
    group.shuffle("pc", &list(), true, 7).unwrap();
    let before = now(&group).clone();

    group.extend("phone", (7, 1), (7, 2), &[4, 5]).unwrap();

    let after = now(&group);
    assert_eq!((after.queue_id, after.revision), (7, 2));
    assert_eq!(after.entry_id, before.entry_id);
    assert_eq!(after.anchor_wall_us, before.anchor_wall_us);
    assert_eq!(after.position_us, before.position_us);
    assert_eq!(after.order[..3], before.order[..]);
    assert_eq!(after.order[3..], [4, 5]);
}

/// 续的不是组此刻那一版(期间有人点了别的歌),或者发的不是成员:拒掉,不往别人的队列里续。
#[test]
fn extending_a_stale_revision_is_refused() {
    let mut group = group();

    assert_eq!(
        group.extend("phone", (7, 0), (7, 2), &[4]),
        Err(Refusal::Stale)
    );
    assert_eq!(
        group.extend("stranger", (7, 1), (7, 2), &[4]),
        Err(Refusal::NotMember)
    );
    assert_eq!(now(&group).revision, 1);
    assert!(
        now(&group).order.is_empty(),
        "不随机时次序仍是原序"
    );
}

/// 从本机正在放的那一份接着:位置、在不在放照原样,锚在「现在」,不另加 LEAD。
#[test]
fn a_seed_carries_on_from_the_local_playback() {
    let mut group = Group::default();
    group.set_outputs("pc", vec!["pc".to_owned()]);

    group
        .seed(
            (7, 1),
            &list(),
            2,
            (30 * SEC as u64, true),
            5 * SEC,
        )
        .unwrap();

    let now = now(&group);
    assert_eq!(now.entry_id, 2);
    assert_eq!(now.anchor_wall_us, 5 * SEC);
    assert_eq!(
        now.position_at(6 * SEC, TRACK),
        31 * SEC as u64
    );
}

/// pc 与 tab 一起出声,在放第 1 条(还没开走)。
fn two_outputs() -> Group {
    let mut group = group();
    group.set_outputs(
        "phone",
        vec!["pc".to_owned(), "tab".to_owned()],
    );
    group
}

fn ids(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

/// 在线的出声设备都报了就绪,起播提前到「那一刻 + LEAD」;缺一台就接着等(#154)。
/// 已经提前过的,再报一遍不再挪。
#[test]
fn the_start_moves_up_once_every_online_output_is_ready() {
    let mut group = two_outputs();
    let version = group.version;
    let online = |_: &str| true;

    assert_eq!(
        group.ready(
            "pc",
            &ids(&["pc"]),
            1,
            version,
            online,
            SEC / 10
        ),
        Ok(false),
        "tab 还没好"
    );
    assert_eq!(now(&group).anchor_wall_us, START_WAIT_US);

    assert_eq!(
        group.ready(
            "tab",
            &ids(&["pc", "tab"]),
            1,
            version,
            online,
            SEC / 5
        ),
        Ok(true)
    );
    assert_eq!(
        now(&group).anchor_wall_us,
        SEC / 5 + LEAD_US
    );
    assert_eq!(now(&group).position_us, 0);

    assert_eq!(
        group.ready(
            "tab",
            &ids(&["pc", "tab"]),
            1,
            version,
            online,
            SEC / 5 + 1
        ),
        Ok(false),
        "已经提前过"
    );
}

/// 掉线的出声设备不等;报的不是此刻那一版、那一条不算数;只当遥控器的报不算数。
#[test]
fn an_offline_output_does_not_hold_the_start() {
    let mut group = two_outputs();
    let version = group.version;
    let only_pc = |id: &str| id == "pc";

    assert_eq!(
        group.ready(
            "pc",
            &ids(&["pc"]),
            2,
            version,
            only_pc,
            SEC / 10
        ),
        Ok(false),
        "不是此刻那一条"
    );
    assert_eq!(
        group.ready(
            "pc",
            &ids(&["pc"]),
            1,
            version - 1,
            only_pc,
            SEC / 10
        ),
        Ok(false),
        "不是此刻那一版"
    );
    assert_eq!(
        group.ready(
            "phone",
            &ids(&["phone"]),
            1,
            version,
            only_pc,
            SEC / 10
        ),
        Err(Refusal::NotMember)
    );
    assert_eq!(now(&group).anchor_wall_us, START_WAIT_US);

    assert_eq!(
        group.ready(
            "pc",
            &ids(&["pc"]),
            1,
            version,
            only_pc,
            SEC / 10
        ),
        Ok(true),
        "tab 掉线了,不等它"
    );
    assert_eq!(
        now(&group).anchor_wall_us,
        SEC / 10 + LEAD_US
    );
}

/// 一台卡住、一直不报:到上限照常开播,不永远等(#154)。
#[test]
fn a_stuck_output_only_delays_the_start_up_to_the_cap() {
    let group = two_outputs();

    assert_eq!(
        now(&group).position_at(START_WAIT_US + SEC, TRACK),
        SEC as u64
    );
}
