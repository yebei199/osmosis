//! `timeline.rs` 的测试:全局播放状态的纯规则。

use contract::LoopModeDto;

use super::timeline::*;

const SEC: i64 = 1_000_000;
const TRACK: u64 = 100 * SEC as u64;

/// 三首,各 100 秒。
fn list() -> Playlist {
    Playlist {
        entries: vec![(1, TRACK), (2, TRACK), (3, TRACK)],
    }
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

/// 点歌锚在「现在 + LEAD」,大家那一刻一起起播。
#[test]
fn a_jump_starts_together_a_moment_from_now() {
    let group = group();

    assert_eq!(now(&group).anchor_wall_us, LEAD_US);
    assert_eq!(now(&group).position_at(0, TRACK), 0);
    assert_eq!(
        now(&group).position_at(LEAD_US + 2 * SEC, TRACK),
        2 * SEC as u64
    );
}

/// 暂停停在那一刻的位置,继续从那里接着放。
#[test]
fn pause_and_resume_keep_the_position() {
    let mut group = group();

    group.pause("pc", &list(), LEAD_US + 10 * SEC).unwrap();
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
        .step("phone", &list(), -1, 3 * SEC + LEAD_US + SEC)
        .unwrap();
    assert_eq!(
        now(&group).entry_id,
        2,
        "刚放一秒,上一首就是前一首"
    );

    let later = 3 * SEC + 2 * LEAD_US + 10 * SEC;
    group.step("phone", &list(), -1, later).unwrap();
    assert_eq!(
        now(&group).entry_id,
        2,
        "放过三秒就回到开头"
    );
    assert_eq!(now(&group).anchor_wall_us, later + LEAD_US);
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

/// 放完自动往下推,下一首锚在上一首真正结束的那一刻。
#[test]
fn a_finished_track_rolls_to_the_next_at_its_end() {
    let mut group = group();
    let end = LEAD_US + 100 * SEC;

    assert!(!group.roll(&list(), end - 1));
    assert!(group.roll(&list(), end + 5 * SEC));

    assert_eq!(now(&group).entry_id, 2);
    assert_eq!(now(&group).anchor_wall_us, end);
    assert_eq!(
        now(&group).position_at(end + 5 * SEC, TRACK),
        5 * SEC as u64
    );
}

/// 不循环放到队尾就停在最后一首的末尾;停在队尾后继续从这一首开头放;单曲循环一直放同一首。
#[test]
fn rolling_stops_at_the_tail_or_repeats_one() {
    let mut group = group();
    group.jump("pc", (7, 1), &list(), 3, 0, 1).unwrap();
    group.roll(&list(), LEAD_US + 150 * SEC);
    assert!(!now(&group).playing);
    assert_eq!(now(&group).position_us, TRACK);
    group.resume("pc", &list(), 200 * SEC).unwrap();
    assert_eq!(now(&group).position_us, 0);

    let mut group = group_with_loop(LoopModeDto::One);
    group.roll(&list(), LEAD_US + 250 * SEC);
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
        LEAD_US + 10 * SEC
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
