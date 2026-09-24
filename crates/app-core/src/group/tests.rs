//! 成员一侧的播放组规则(#137 ⑤)。

use similar_asserts::assert_eq;

use super::*;

const S: u64 = 1_000_000;

fn track(id: &str, seconds: u64) -> TrackDto {
    TrackDto {
        platform: "netease".to_owned(),
        id: id.to_owned(),
        title: id.to_uppercase(),
        alias: None,
        artists: vec!["LiSA".to_owned()],
        cover: None,
        duration_ms: (seconds * 1_000) as i64,
    }
}

fn draft(entry_id: i64, next: Option<i64>) -> Draft {
    Draft {
        clock_epoch: 7,
        queue_id: 3,
        revision: 2,
        entry_id,
        track: track(&format!("t{entry_id}"), 200),
        next: next
            .map(|id| (id, track(&format!("t{id}"), 100))),
        play_order: None,
        round: 0,
        shuffled: false,
        loop_mode: LoopModeDto::Off,
    }
}

/// 本机 "me" 在一个三台的组里、已经开始跟着放，主端是 `master`。
fn in_group(master: &str) -> Group {
    let mut group = Group::new("me");
    group.on_group(
        4,
        Some(master.to_owned()),
        vec![
            "a".to_owned(),
            "me".to_owned(),
            "b".to_owned(),
        ],
        0,
    );
    group.join();
    group
}

/// 主端 "a" 在任期 4 写的第 `seq` 份：服务端时钟 10s 那一刻从 5s 起播，放到 300s。
fn plan(seq: u64) -> GroupPlanDto {
    GroupPlanDto {
        seq,
        clock_epoch: 7,
        queue_id: 3,
        revision: 2,
        entry_id: 11,
        track: track("t11", 200),
        anchor_us: 10 * S,
        position_us: 5 * S,
        playing: true,
        start_us: 10 * S,
        next: None,
        valid_until_us: 205 * S,
        play_order: None,
        round: 0,
        shuffled: false,
        loop_mode: LoopModeDto::Off,
    }
}

// ── 身份 ──

/// 不在组里、组里只有自己：照本机自己的放。
#[test]
fn outside_a_multi_member_group_the_device_plays_on_its_own()
 {
    let mut group = Group::new("me");
    assert_eq!(group.role(), GroupRole::Solo);
    assert_eq!(group.verdict(0, 0), Verdict::Solo);

    group.on_group(
        1,
        Some("me".to_owned()),
        vec!["me".to_owned()],
        0,
    );
    assert_eq!(
        group.role(),
        GroupRole::Solo,
        "一台的组不必同步"
    );
}

/// 还在准备、没被叫开始：手上原来在放的不动，计划先收着。
#[test]
fn a_member_that_has_not_started_keeps_its_own_playback() {
    let mut group = Group::new("me");
    group.on_group(
        4,
        Some("a".to_owned()),
        vec!["a".to_owned(), "me".to_owned()],
        0,
    );
    assert!(group.on_plan(4, plan(1), 0), "计划照收");

    assert_eq!(group.verdict(11 * S, 0), Verdict::Solo);
    group.join();
    assert!(matches!(
        group.verdict(11 * S, 0),
        Verdict::Follow(_)
    ));
}

/// 通告里没有本机：离组，手上的计划一并忘掉。
#[test]
fn a_group_without_this_device_is_a_leave() {
    let mut group = in_group("a");
    assert!(group.on_plan(4, plan(1), 0));

    group.on_group(
        5,
        Some("a".to_owned()),
        vec!["a".to_owned(), "b".to_owned()],
        10,
    );

    assert_eq!(group.role(), GroupRole::Solo);
    assert_eq!(group.plan(), None);
}

/// 旧任期的通告迟到了：不理。
#[test]
fn a_late_announcement_of_an_old_term_is_ignored() {
    let mut group = in_group("a");
    group.on_group(
        3,
        Some("me".to_owned()),
        vec!["me".to_owned(), "x".to_owned()],
        10,
    );

    assert_eq!(group.role(), GroupRole::Follower);
    assert_eq!(group.term(), Some(4));
}

// ── 跟随端收计划 ──

/// 收到计划之前别出声;收到之后照它放。
#[test]
fn a_follower_waits_for_a_plan_then_follows_it() {
    let mut group = in_group("a");
    assert_eq!(group.verdict(11 * S, 0), Verdict::Waiting);

    assert!(group.on_plan(4, plan(1), 0));

    let Verdict::Follow(now) = group.verdict(11 * S, 0)
    else {
        panic!("该照计划放");
    };
    assert_eq!(
        (now.entry_id, now.anchor_us, now.position_us),
        (11, 10 * S, 5 * S)
    );
}

/// 旧任期、旧序号的迟到计划不生效;一模一样的那份是心跳，不算新的。
#[test]
fn stale_plans_are_refused_and_repeats_are_heartbeats() {
    let mut group = in_group("a");
    assert!(group.on_plan(4, plan(2), 0));

    assert!(!group.on_plan(3, plan(9), 10), "旧任期");
    assert!(!group.on_plan(4, plan(1), 10), "旧序号");
    assert!(!group.on_plan(4, plan(2), 10), "心跳");
    assert_eq!(group.plan().map(|plan| plan.seq), Some(2));
    assert!(
        group.on_plan(5, plan(1), 20),
        "新任期的第一份照收"
    );
}

/// 次序只在变了的时候带;没带的沿用手上那份(交接时新主端照它接着放)。
#[test]
fn a_plan_without_an_order_keeps_the_last_one() {
    let mut group = in_group("a");
    group.on_plan(
        4,
        GroupPlanDto {
            play_order: Some(vec![11, 13, 12]),
            ..plan(1)
        },
        0,
    );

    group.on_plan(
        4,
        GroupPlanDto {
            position_us: 9 * S,
            ..plan(2)
        },
        10,
    );

    assert_eq!(
        group
            .plan()
            .and_then(|plan| plan.play_order.clone()),
        Some(vec![11, 13, 12])
    );
}

/// 预告的下一首到点就换过去，从它的开头放 —— 不等主端再发一份。
#[test]
fn the_announced_next_entry_takes_over_at_its_moment() {
    let mut group = in_group("a");
    let mut announced = plan(1);
    announced.next = Some(NextEntryDto {
        entry_id: 12,
        track: track("t12", 100),
        at_us: 205 * S,
    });
    announced.valid_until_us = 305 * S;
    group.on_plan(4, announced, 0);

    let Verdict::Follow(before) = group.verdict(204 * S, 0)
    else {
        panic!("该照计划放");
    };
    assert_eq!(before.entry_id, 11);
    let Verdict::Follow(after) = group.verdict(206 * S, 0)
    else {
        panic!("该照计划放");
    };
    assert_eq!(
        (
            after.entry_id,
            after.anchor_us,
            after.position_us,
            after.start_us
        ),
        (12, 205 * S, 0, 205 * S)
    );
}

// ── 主端失联 ──

/// 主端三秒没动静算失联，但照旧按计划放;放到有效期末尾停下，并说明是主端失联。
#[test]
fn after_the_master_goes_silent_the_plan_runs_out_then_stops()
 {
    let mut group = in_group("a");
    group.on_plan(4, plan(1), 1_000);

    assert!(!group.master_silent(1_000 + MASTER_SILENT_MS));
    assert!(
        group.master_silent(1_000 + MASTER_SILENT_MS + 1)
    );
    assert!(
        matches!(
            group.verdict(100 * S, 10_000),
            Verdict::Follow(_)
        ),
        "失联了也把已确认的放完"
    );
    assert_eq!(
        group.verdict(205 * S, 10_000),
        Verdict::Expired
    );
}

/// 主端的心跳一直在：过了名义上的有效期也接着照计划放(元数据时长与真实媒体常差一两秒),
/// 不比主端先停。
#[test]
fn a_live_master_keeps_the_plan_going_past_its_nominal_end()
{
    let mut group = in_group("a");
    group.on_plan(4, plan(1), 1_000);
    group.on_plan(4, plan(1), 200_000);

    assert!(matches!(
        group.verdict(206 * S, 200_500),
        Verdict::Follow(_)
    ));
}

/// 暂停着的计划没有「到头」:停在锚点那个位置等着。
#[test]
fn a_paused_plan_never_runs_out() {
    let mut group = in_group("a");
    group.on_plan(
        4,
        GroupPlanDto {
            playing: false,
            valid_until_us: 10 * S,
            ..plan(1)
        },
        0,
    );

    assert!(
        matches!(group.verdict(999 * S, 0), Verdict::Follow(ref now) if !now.playing)
    );
}

// ── 主端写计划 ──

/// 不是主端写不了计划。
#[test]
fn only_the_master_publishes() {
    let mut group = in_group("a");
    assert_eq!(
        group.publish(
            draft(11, None),
            Cue::Start { position_us: 0 },
            0,
            0
        ),
        None
    );
}

/// 一起开始锚在现在之后 LEAD_US:大家在同一刻出声;主端自己也照这一份放。
#[test]
fn a_start_anchors_the_timeline_a_little_ahead() {
    let mut group = in_group("me");

    let (term, plan) = group
        .publish(
            draft(11, Some(12)),
            Cue::Start { position_us: 5 * S },
            100 * S,
            0,
        )
        .expect("主端该写得出计划");

    assert_eq!(term, 4);
    assert_eq!(plan.seq, 1);
    assert_eq!(
        (plan.anchor_us, plan.start_us),
        (100 * S + LEAD_US, 100 * S + LEAD_US)
    );
    assert_eq!(plan.position_us, 5 * S);
    assert_eq!(plan.next, None, "还剩三分多钟，不预告");
    assert_eq!(
        plan.valid_until_us,
        100 * S + LEAD_US + 195 * S,
        "管到这一首放完"
    );
    assert!(matches!(
        group.verdict(101 * S, 0),
        Verdict::Follow(_)
    ));
}

/// 本机实际播放离计划不到 REANCHOR_US:沿用原来的锚点，序号不变(跟随端不必重新对准)。
#[test]
fn playing_close_to_the_plan_keeps_the_anchor() {
    let mut group = in_group("me");
    let (_, first) = group
        .publish(
            draft(11, Some(12)),
            Cue::Start { position_us: 0 },
            0,
            0,
        )
        .unwrap();

    let at = LEAD_US + 30 * S;
    let (_, kept) = group
        .publish(
            draft(11, Some(12)),
            Cue::Playing {
                at_us: at,
                position_us: 30 * S + REANCHOR_US,
            },
            at,
            30_000,
        )
        .unwrap();

    assert_eq!(kept, first);
}

/// 偏离超过 REANCHOR_US(跳转过、漂够了):按实测重新定锚，序号加一。
#[test]
fn playing_off_the_plan_reanchors_it() {
    let mut group = in_group("me");
    group
        .publish(
            draft(11, Some(12)),
            Cue::Start { position_us: 0 },
            0,
            0,
        )
        .unwrap();

    let at = LEAD_US + 30 * S;
    let (_, moved) = group
        .publish(
            draft(11, Some(12)),
            Cue::Playing {
                at_us: at,
                position_us: 90 * S,
            },
            at,
            30_000,
        )
        .unwrap();

    assert_eq!(moved.seq, 2);
    assert_eq!(
        (moved.anchor_us, moved.position_us),
        (at, 90 * S)
    );
}

/// 心跳原样再发(序号不变);暂停是一份新的，同一位置再报暂停还是那一份。
#[test]
fn a_heartbeat_repeats_the_plan_and_a_pause_bumps_the_sequence()
 {
    let mut group = in_group("me");
    let (_, first) = group
        .publish(
            draft(11, Some(12)),
            Cue::Start { position_us: 0 },
            0,
            0,
        )
        .unwrap();

    let (_, beat) = group
        .publish(draft(11, Some(12)), Cue::Keep, S, 1_000)
        .unwrap();
    assert_eq!(beat, first);

    let paused = |group: &mut Group, now| {
        group
            .publish(
                draft(11, Some(12)),
                Cue::Paused {
                    position_us: 30 * S,
                },
                now,
                0,
            )
            .unwrap()
            .1
    };
    let once = paused(&mut group, 31 * S);
    assert_eq!(
        (once.seq, once.playing, once.position_us),
        (2, false, 30 * S)
    );
    assert_eq!(
        paused(&mut group, 32 * S),
        once,
        "停着没动就是同一份"
    );
}

/// 快放完时预告下一首，有效期跟着延到下一首末尾;主端切过去时离预告那一刻不远就沿用它。
#[test]
fn the_next_entry_is_announced_near_the_end_and_taken_at_that_moment()
 {
    let mut group = in_group("me");
    group
        .publish(
            draft(11, Some(12)),
            Cue::Start { position_us: 0 },
            0,
            0,
        )
        .unwrap();
    let end = LEAD_US + 200 * S;

    let (_, early) = group
        .publish(
            draft(11, Some(12)),
            Cue::Keep,
            end - PREANNOUNCE_US - S,
            0,
        )
        .unwrap();
    assert_eq!(early.next, None);

    let (_, near) = group
        .publish(
            draft(11, Some(12)),
            Cue::Keep,
            end - PREANNOUNCE_US,
            0,
        )
        .unwrap();
    let next = near.next.clone().expect("该预告下一首了");
    assert_eq!((next.entry_id, next.at_us), (12, end));
    assert_eq!(near.valid_until_us, end + 100 * S);

    let (_, switched) = group
        .publish(
            draft(12, None),
            Cue::Playing {
                at_us: end + S,
                position_us: S + 1_000,
            },
            end + S,
            0,
        )
        .unwrap();
    assert_eq!(
        (
            switched.entry_id,
            switched.anchor_us,
            switched.position_us
        ),
        (12, end, 0),
        "主端晚一点点换过去，仍沿用预告的那一刻"
    );
}

// ── 主端实测的平滑 ──

/// 单个呈现时刻带着测量抖动:取最近几个截距的中位数，抖动不烙进全组的时间线。
#[test]
fn intercepts_take_the_median_of_recent_measurements() {
    let mut intercepts = Intercepts::default();
    // 真实截距 100s:服务端时刻 − 媒体位置。测量噪声 ±1.5ms,有一个 +3ms 的离群值。
    let noise = [0, 1_500, -1_500, 3_000, -500, 500, 0];
    let mut last = 0;
    for (i, n) in noise.iter().enumerate() {
        let at = 200 * S + i as u64 * 200_000;
        last = intercepts.push(
            at,
            (at as i64 - 100 * S as i64 + n) as u64,
        );
    }
    let at = 200 * S + 6 * 200_000;
    assert_eq!(
        last,
        at - 100 * S,
        "中位数把噪声与离群值都滤掉了"
    );
}

/// 截距一下子变了很多(跳转、换歌、缓冲之后):旧的作废，从这一个重新攒。
#[test]
fn a_jump_in_the_intercept_starts_over() {
    let mut intercepts = Intercepts::default();
    for i in 0..5 {
        let at = 10 * S + i * 200_000;
        intercepts.push(at, at - 5 * S);
    }

    let at = 11 * S;
    let after_seek = intercepts.push(at, 60 * S);

    assert_eq!(after_seek, 60 * S);
}

/// 显式交接：跟随端成了新主端，第一份沿用上一任的时间线，只换任期、序号从 1 起;
/// 播放次序随计划走，新主端照着接着放。
#[test]
fn a_new_master_carries_on_the_old_timeline() {
    let mut group = in_group("a");
    let mut held = plan(6);
    held.play_order = Some(vec![11, 13, 12]);
    held.shuffled = true;
    group.on_plan(4, held, 0);

    group.on_group(
        5,
        Some("me".to_owned()),
        vec!["me".to_owned(), "b".to_owned()],
        50,
    );
    assert_eq!(group.role(), GroupRole::Master);
    assert!(!group.master_silent(10_000), "自己就是主端");

    let order = group
        .plan()
        .and_then(|plan| plan.play_order.clone());
    let mut carried = draft(11, Some(13));
    carried.play_order = order;
    carried.shuffled = true;
    let (term, plan) = group
        .publish(carried, Cue::Keep, 20 * S, 60)
        .unwrap();

    assert_eq!((term, plan.seq), (5, 1));
    assert_eq!(
        (plan.anchor_us, plan.position_us),
        (10 * S, 5 * S),
        "不重新定锚"
    );
    assert_eq!(plan.play_order, Some(vec![11, 13, 12]));
}
