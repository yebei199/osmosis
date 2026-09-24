//! ③ 的单成员迁移：选一台设备 = 把当前播放整个迁过去(#137 ③)。

use contract::DeviceDto;
use similar_asserts::assert_eq;

use super::*;

const NOW: u64 = 1_000_000;

fn device(id: &str) -> Output {
    Output::Remote(DeviceDto {
        id: id.to_owned(),
        name: format!("设备 {id}"),
    })
}

fn track() -> TrackDto {
    TrackDto {
        platform: "netease".to_owned(),
        id: "a".to_owned(),
        title: "A".to_owned(),
        alias: None,
        artists: vec!["LiSA".to_owned()],
        cover: None,
        duration_ms: 240_000,
    }
}

fn plan() -> Plan {
    Plan {
        queue_id: 7,
        revision: 3,
        entry_id: 12,
        position_ms: 61_500,
        playing: true,
        track: track(),
    }
}

fn ack(
    op: &str,
    phase: OperationPhase,
    position_ms: Option<u64>,
) -> OperationAckDto {
    OperationAckDto {
        operation_id: op.to_owned(),
        phase,
        position_ms,
        reason: None,
    }
}

fn failed(op: &str, reason: &str) -> OperationAckDto {
    OperationAckDto {
        operation_id: op.to_owned(),
        phase: OperationPhase::Failed,
        position_ms: None,
        reason: Some(reason.to_owned()),
    }
}

/// 一个输出在 `output` 上的会话。
fn session_at(output: Output) -> Session {
    Session {
        members: vec![output.clone()],
        master: output,
        ..Session::default()
    }
}

/// 本机 → pc1 的迁移已经走到「源在停」那一步。
fn stopping_local_to_pc1() -> Session {
    let mut session = Session::default();
    session
        .begin(
            "op".to_owned(),
            device("pc1"),
            Some(plan()),
            NOW,
        )
        .expect("第一次迁移该开得起来");
    session.on_ack(
        &device("pc1"),
        &ack("op", OperationPhase::Prepared, None),
        NOW + 100,
    );
    session
}

/// 同上,走到「目标在起」。源停在 63 秒。
fn starting_local_to_pc1() -> Session {
    let mut session = stopping_local_to_pc1();
    session.on_ack(
        &Output::Local,
        &ack(
            "op",
            OperationPhase::Stopped,
            Some(63_000),
        ),
        NOW + 200,
    );
    session
}

// ── 正常路径 ──

/// 开始一次迁移:先向服务端登记新的输出集合,再叫目标准备 —— 源一个字节都不动。
#[test]
fn beginning_a_move_registers_the_outputs_and_prepares_the_target()
 {
    let mut session = Session::default();

    let effects = session
        .begin(
            "op".to_owned(),
            device("pc1"),
            Some(plan()),
            NOW,
        )
        .expect("第一次迁移该开得起来");

    assert_eq!(
        effects,
        vec![
            Effect::Begin {
                operation_id: "op".to_owned(),
                outputs: vec!["pc1".to_owned()],
                master: Some("pc1".to_owned()),
            },
            Effect::Prepare {
                operation_id: "op".to_owned(),
                to: device("pc1"),
                plan: plan(),
            },
        ]
    );
    assert_eq!(
        session.output(),
        &Output::Local,
        "确认之前输出不换"
    );
    assert_eq!(
        session.moving().map(|m| m.phase),
        Some(Phase::Running(Step::Preparing))
    );
}

/// 四步走完:准备好 → 停源 → 从源停下的那一毫秒开始 → 确认,输出这才换过去。
///
/// 锚点是源**停下时**报的 63000,不是发起时的 61500:准备那几秒源一直在放,
/// 按发起时的位置开始就等于让用户把那几秒再听一遍。
#[test]
fn a_move_walks_prepare_stop_start_commit_from_the_stop_anchor()
 {
    let mut session = Session::default();
    session
        .begin(
            "op".to_owned(),
            device("pc1"),
            Some(plan()),
            NOW,
        )
        .expect("第一次迁移该开得起来");

    let after_prepared = session.on_ack(
        &device("pc1"),
        &ack("op", OperationPhase::Prepared, None),
        NOW + 100,
    );
    assert_eq!(
        after_prepared,
        vec![Effect::Stop {
            operation_id: "op".to_owned(),
            from: Output::Local,
        }]
    );

    let after_stopped = session.on_ack(
        &Output::Local,
        &ack(
            "op",
            OperationPhase::Stopped,
            Some(63_000),
        ),
        NOW + 200,
    );
    assert_eq!(
        after_stopped,
        vec![Effect::Start {
            operation_id: "op".to_owned(),
            to: device("pc1"),
            position_ms: 63_000,
            playing: true,
        }]
    );
    assert_eq!(
        session.output(),
        &Output::Local,
        "目标还没确认开始,输出不换"
    );

    let after_started = session.on_ack(
        &device("pc1"),
        &ack(
            "op",
            OperationPhase::Started,
            Some(63_000),
        ),
        NOW + 300,
    );
    assert_eq!(
        after_started,
        vec![Effect::Commit {
            operation_id: "op".to_owned(),
            outputs: None,
        }]
    );
    assert_eq!(session.output(), &device("pc1"));
    assert_eq!(session.moving(), None);
}

/// 源停下时没报位置,就按发起那一刻的位置开始 —— 总比从 0:00 强。
#[test]
fn a_stop_without_a_position_starts_from_the_planned_one()
 {
    let mut session = stopping_local_to_pc1();

    let effects = session.on_ack(
        &Output::Local,
        &ack("op", OperationPhase::Stopped, None),
        NOW + 200,
    );

    assert_eq!(
        effects,
        vec![Effect::Start {
            operation_id: "op".to_owned(),
            to: device("pc1"),
            position_ms: plan().position_ms,
            playing: true,
        }]
    );
}

/// 源是暂停着的:目标开始时也停在起点,不自己响起来。
#[test]
fn a_paused_source_starts_the_target_paused() {
    let mut session = Session::default();
    let paused = Plan {
        playing: false,
        ..plan()
    };
    session
        .begin(
            "op".to_owned(),
            device("pc1"),
            Some(paused),
            NOW,
        )
        .expect("第一次迁移该开得起来");
    session.on_ack(
        &device("pc1"),
        &ack("op", OperationPhase::Prepared, None),
        NOW + 100,
    );

    let effects = session.on_ack(
        &Output::Local,
        &ack(
            "op",
            OperationPhase::Stopped,
            Some(61_500),
        ),
        NOW + 200,
    );

    assert_eq!(
        effects,
        vec![Effect::Start {
            operation_id: "op".to_owned(),
            to: device("pc1"),
            position_ms: 61_500,
            playing: false,
        }]
    );
}

/// 远端 A → 远端 B:停的是 A,准备与开始的是 B,登记的集合只有 B。
#[test]
fn a_remote_to_remote_move_stops_the_old_device_and_starts_the_new_one()
 {
    let mut session = session_at(device("a"));

    let begun = session
        .begin(
            "op".to_owned(),
            device("b"),
            Some(plan()),
            NOW,
        )
        .expect("迁移该开得起来");
    assert_eq!(
        begun,
        vec![
            Effect::Begin {
                operation_id: "op".to_owned(),
                outputs: vec!["b".to_owned()],
                master: Some("b".to_owned()),
            },
            Effect::Prepare {
                operation_id: "op".to_owned(),
                to: device("b"),
                plan: plan(),
            },
        ]
    );

    let stop = session.on_ack(
        &device("b"),
        &ack("op", OperationPhase::Prepared, None),
        NOW + 100,
    );
    assert_eq!(
        stop,
        vec![Effect::Stop {
            operation_id: "op".to_owned(),
            from: device("a"),
        }]
    );

    let start = session.on_ack(
        &device("a"),
        &ack(
            "op",
            OperationPhase::Stopped,
            Some(63_000),
        ),
        NOW + 200,
    );
    assert_eq!(
        start,
        vec![Effect::Start {
            operation_id: "op".to_owned(),
            to: device("b"),
            position_ms: 63_000,
            playing: true,
        }]
    );
}

/// 远端 → 本机:登记的集合是空的(本机不经服务端),准备的是本机。
#[test]
fn moving_back_home_registers_no_outputs_and_prepares_locally()
 {
    let mut session = session_at(device("a"));

    let begun = session
        .begin(
            "op".to_owned(),
            Output::Local,
            Some(plan()),
            NOW,
        )
        .expect("迁移该开得起来");

    assert_eq!(
        begun,
        vec![
            Effect::Begin {
                operation_id: "op".to_owned(),
                outputs: Vec::new(),
                master: None,
            },
            Effect::Prepare {
                operation_id: "op".to_owned(),
                to: Output::Local,
                plan: plan(),
            },
        ]
    );
}

/// 什么都没在放:没有东西可迁,不准备、不开始,只停源、换输出。
///
/// 「被移除的成员停止实际音频输出」照样成立 —— 远端 A 改回本机时 A 仍要停,
/// 否则它就成了一台没人管、还在响的设备(#137 ① F5)。
#[test]
fn a_move_with_nothing_playing_only_stops_the_source() {
    let mut session = session_at(device("a"));

    let begun = session
        .begin(
            "op".to_owned(),
            Output::Local,
            None,
            NOW,
        )
        .expect("迁移该开得起来");
    assert_eq!(
        begun,
        vec![
            Effect::Begin {
                operation_id: "op".to_owned(),
                outputs: Vec::new(),
                master: None,
            },
            Effect::Stop {
                operation_id: "op".to_owned(),
                from: device("a"),
            },
        ]
    );

    let committed = session.on_ack(
        &device("a"),
        &ack("op", OperationPhase::Stopped, Some(0)),
        NOW + 100,
    );
    assert_eq!(
        committed,
        vec![Effect::Commit {
            operation_id: "op".to_owned(),
            outputs: None,
        }]
    );
    assert_eq!(session.output(), &Output::Local);
}

/// 服务端回的任期只认最后一次提交的那个操作。
#[test]
fn the_term_comes_only_from_the_committed_operation() {
    let mut session = starting_local_to_pc1();
    session.on_ack(
        &device("pc1"),
        &ack("op", OperationPhase::Started, None),
        NOW + 300,
    );

    session.committed("older", 9);
    assert_eq!(
        session.term(),
        0,
        "别的操作回的任期不算数"
    );

    session.committed("op", 4);
    assert_eq!(session.term(), 4);
}

// ── 不开始的情形 ──

/// 选的就是现在的输出:什么都不做。
#[test]
fn selecting_the_current_output_is_refused() {
    let mut session = session_at(device("pc1"));

    assert_eq!(
        session.begin(
            "op".to_owned(),
            device("pc1"),
            Some(plan()),
            NOW
        ),
        Err(Refused::AlreadyThere)
    );
    assert_eq!(session.moving(), None);
}

/// 源已经在停了:不许半路换目标。
#[test]
fn a_new_target_is_refused_once_the_source_is_stopping()
{
    let mut session = stopping_local_to_pc1();

    assert_eq!(
        session.begin(
            "op2".to_owned(),
            device("tv"),
            Some(plan()),
            NOW + 150
        ),
        Err(Refused::Busy)
    );
    assert_eq!(
        session
            .moving()
            .map(|m| m.operation_id.clone()),
        Some("op".to_owned()),
        "进行中的那一次原样留着"
    );
}

// ── AC-3.3:有效期 ──

/// 准备途中换目标:旧目标撤掉(取消 + 作罢),新目标重新登记、准备。
#[test]
fn a_new_target_during_prepare_supersedes_the_old_one()
{
    let mut session = Session::default();
    session
        .begin(
            "op1".to_owned(),
            device("pc1"),
            Some(plan()),
            NOW,
        )
        .expect("第一次迁移该开得起来");

    let effects = session
        .begin(
            "op2".to_owned(),
            device("tv"),
            Some(plan()),
            NOW + 50,
        )
        .expect("准备途中可以换目标");

    assert_eq!(
        effects,
        vec![
            Effect::Cancel {
                operation_id: "op1".to_owned(),
                to: device("pc1"),
            },
            Effect::Abort {
                operation_id: "op1".to_owned()
            },
            Effect::Begin {
                operation_id: "op2".to_owned(),
                outputs: vec!["tv".to_owned()],
                master: Some("tv".to_owned()),
            },
            Effect::Prepare {
                operation_id: "op2".to_owned(),
                to: device("tv"),
                plan: plan(),
            },
        ]
    );
}

/// 被顶掉的那一次迟到的「准备好了」不推进任何东西。
#[test]
fn a_late_ready_from_a_superseded_move_is_ignored() {
    let mut session = Session::default();
    session
        .begin(
            "op1".to_owned(),
            device("pc1"),
            Some(plan()),
            NOW,
        )
        .expect("第一次迁移该开得起来");
    session
        .begin(
            "op2".to_owned(),
            device("tv"),
            Some(plan()),
            NOW + 50,
        )
        .expect("准备途中可以换目标");

    let effects = session.on_ack(
        &device("pc1"),
        &ack("op1", OperationPhase::Prepared, None),
        NOW + 100,
    );

    assert_eq!(effects, Vec::new());
    assert_eq!(
        session.moving().map(|m| m.phase),
        Some(Phase::Running(Step::Preparing))
    );
}

/// 回话来自不该回这一步的那台,不算数:源说「准备好了」推进不了准备。
#[test]
fn an_ack_from_the_wrong_device_is_ignored() {
    let mut session = session_at(device("a"));
    session
        .begin(
            "op".to_owned(),
            device("b"),
            Some(plan()),
            NOW,
        )
        .expect("迁移该开得起来");

    let effects = session.on_ack(
        &device("a"),
        &ack("op", OperationPhase::Prepared, None),
        NOW + 100,
    );

    assert_eq!(effects, Vec::new());
}

/// 迁移已经结束,之后迟到的 ready / start 一概不理。
#[test]
fn acks_after_a_finished_move_change_nothing() {
    let mut session = starting_local_to_pc1();
    session.on_ack(
        &device("pc1"),
        &ack("op", OperationPhase::Started, None),
        NOW + 300,
    );

    for late in [
        ack("op", OperationPhase::Prepared, None),
        ack("op", OperationPhase::Started, None),
        ack("op", OperationPhase::Stopped, Some(1)),
    ] {
        assert_eq!(
            session.on_ack(
                &device("pc1"),
                &late,
                NOW + 400
            ),
            Vec::new()
        );
    }
    assert_eq!(session.output(), &device("pc1"));
}

/// 同一步的重复回话只推进一次:两条「停了」只换来一次开始。
#[test]
fn a_duplicate_stop_starts_the_target_only_once() {
    let mut session = stopping_local_to_pc1();
    let stopped = ack(
        "op",
        OperationPhase::Stopped,
        Some(63_000),
    );

    let first = session.on_ack(
        &Output::Local,
        &stopped,
        NOW + 200,
    );
    let second = session.on_ack(
        &Output::Local,
        &stopped,
        NOW + 210,
    );

    assert_eq!(first.len(), 1);
    assert_eq!(
        second,
        Vec::new(),
        "重复的停止确认不该再开始一次"
    );
}

// ── AC-3.2:结果不确定 ──

/// 源停止的确认没到:进「待确认」,**不启动目标**。
#[test]
fn an_unconfirmed_stop_never_starts_the_target() {
    let mut session = stopping_local_to_pc1();

    let at_deadline =
        session.tick(NOW + 100 + STOP_TIMEOUT_MS + 1);
    let later = session.tick(NOW + 60_000);

    assert_eq!(at_deadline, Vec::new());
    assert_eq!(later, Vec::new());
    assert_eq!(
        session.moving().map(|m| m.phase),
        Some(Phase::Unconfirmed(Doubt::SourceStop))
    );
    assert_eq!(session.output(), &Output::Local);
}

/// 停止的确认迟到了:照收,目标这才开始 —— 而且只开始一次。
#[test]
fn a_late_stop_after_the_doubt_starts_the_target_once()
{
    let mut session = stopping_local_to_pc1();
    session.tick(NOW + 100 + STOP_TIMEOUT_MS + 1);
    let stopped = ack(
        "op",
        OperationPhase::Stopped,
        Some(63_000),
    );

    let first = session.on_ack(
        &Output::Local,
        &stopped,
        NOW + 9_000,
    );
    let again = session.on_ack(
        &Output::Local,
        &stopped,
        NOW + 9_100,
    );

    assert_eq!(
        first,
        vec![Effect::Start {
            operation_id: "op".to_owned(),
            to: device("pc1"),
            position_ms: 63_000,
            playing: true,
        }]
    );
    assert_eq!(again, Vec::new());
}

/// 目标开始的确认没到:进「待确认」,**不恢复源**,什么都不发。
#[test]
fn an_unconfirmed_start_never_resumes_the_source() {
    let mut session = starting_local_to_pc1();

    let at_deadline =
        session.tick(NOW + 200 + START_TIMEOUT_MS + 1);
    let later = session.tick(NOW + 60_000);

    assert_eq!(at_deadline, Vec::new());
    assert_eq!(later, Vec::new());
    assert_eq!(
        session.moving().map(|m| m.phase),
        Some(Phase::Unconfirmed(Doubt::TargetStart))
    );
}

/// 开始的确认迟到了:照收并提交 —— 只提交一次,也不重发开始。
#[test]
fn a_late_start_after_the_doubt_commits_once() {
    let mut session = starting_local_to_pc1();
    session.tick(NOW + 200 + START_TIMEOUT_MS + 1);
    let started = ack(
        "op",
        OperationPhase::Started,
        Some(63_000),
    );

    let first = session.on_ack(
        &device("pc1"),
        &started,
        NOW + 20_000,
    );
    let again = session.on_ack(
        &device("pc1"),
        &started,
        NOW + 20_100,
    );

    assert_eq!(
        first,
        vec![Effect::Commit {
            operation_id: "op".to_owned(),
            outputs: None,
        }]
    );
    assert_eq!(again, Vec::new());
    assert_eq!(session.output(), &device("pc1"));
}

/// 在「源停止待确认」上重试:同一个操作号再叫源停一次。
#[test]
fn retrying_an_unconfirmed_stop_resends_the_same_stop()
{
    let mut session = stopping_local_to_pc1();
    session.tick(NOW + 100 + STOP_TIMEOUT_MS + 1);

    let effects = session.retry(NOW + 10_000);

    assert_eq!(
        effects,
        vec![Effect::Stop {
            operation_id: "op".to_owned(),
            from: Output::Local,
        }]
    );
    assert_eq!(
        session.moving().map(|m| m.phase),
        Some(Phase::Running(Step::Stopping))
    );
}

/// 在「目标开始待确认」上重试:同一个操作号、同一个锚点再叫一次开始。
///
/// 目标那头按操作号去重,已经开始过的不会再从锚点起一遍。
#[test]
fn retrying_an_unconfirmed_start_resends_the_same_start()
 {
    let mut session = starting_local_to_pc1();
    session.tick(NOW + 200 + START_TIMEOUT_MS + 1);

    let effects = session.retry(NOW + 20_000);

    assert_eq!(
        effects,
        vec![Effect::Start {
            operation_id: "op".to_owned(),
            to: device("pc1"),
            position_ms: 63_000,
            playing: true,
        }]
    );
}

/// 在「源停止待确认」上放弃:撤掉目标的准备,输出仍是源。不启动目标。
#[test]
fn abandoning_an_unconfirmed_stop_cancels_the_target() {
    let mut session = stopping_local_to_pc1();
    session.tick(NOW + 100 + STOP_TIMEOUT_MS + 1);

    let effects = session.abandon();

    assert_eq!(
        effects,
        vec![
            Effect::Cancel {
                operation_id: "op".to_owned(),
                to: device("pc1"),
            },
            Effect::Abort {
                operation_id: "op".to_owned()
            },
        ]
    );
    assert_eq!(session.output(), &Output::Local);
    assert_eq!(session.moving(), None);
    assert!(
        session.failure().is_some(),
        "放弃了要说一句"
    );
}

/// 在「目标开始待确认」上放弃:叫目标停,**不恢复源**。
///
/// 目标断着网的话这一条停止它收不到 —— 所以界面不能把放弃说成「已经静音」,
/// 那句话在 `failure` 里,由界面照着说。
#[test]
fn abandoning_an_unconfirmed_start_stops_the_target_and_never_resumes_the_source()
 {
    let mut session = starting_local_to_pc1();
    session.tick(NOW + 200 + START_TIMEOUT_MS + 1);

    let effects = session.abandon();

    assert_eq!(
        effects,
        vec![
            Effect::Stop {
                operation_id: "op".to_owned(),
                from: device("pc1"),
            },
            Effect::Abort {
                operation_id: "op".to_owned()
            },
        ]
    );
    assert!(
        !effects.iter().any(|effect| matches!(
            effect,
            Effect::Start { .. }
        )),
        "放弃之后不许有任何一台自动开始"
    );
    assert_eq!(session.moving(), None);
    assert!(
        session
            .failure()
            .is_some_and(|why| why.contains("不能确认")),
        "不能把放弃说成已经静音: {:?}",
        session.failure()
    );
}

/// 还在正常推进时按不了重试也按不了放弃 —— 那两颗键只在「待确认」上出现。
#[test]
fn retry_and_abandon_do_nothing_while_the_move_is_running()
 {
    let mut session = stopping_local_to_pc1();

    assert_eq!(session.retry(NOW + 150), Vec::new());
    assert_eq!(session.abandon(), Vec::new());
    assert_eq!(
        session.moving().map(|m| m.phase),
        Some(Phase::Running(Step::Stopping))
    );
}

// ── 确定的失败 ──

/// 准备超时:这时候什么都还没动,放弃是安全的 —— 撤目标、作罢、源照旧在放。
#[test]
fn a_prepare_that_times_out_gives_up_and_leaves_the_source_alone()
 {
    let mut session = Session::default();
    session
        .begin(
            "op".to_owned(),
            device("pc1"),
            Some(plan()),
            NOW,
        )
        .expect("第一次迁移该开得起来");

    let effects =
        session.tick(NOW + PREPARE_TIMEOUT_MS + 1);

    assert_eq!(
        effects,
        vec![
            Effect::Cancel {
                operation_id: "op".to_owned(),
                to: device("pc1"),
            },
            Effect::Abort {
                operation_id: "op".to_owned()
            },
        ]
    );
    assert_eq!(session.output(), &Output::Local);
    assert_eq!(session.moving(), None);
    assert!(session.failure().is_some());
}

/// 目标说准备不了:作罢,把它说的原因记下来。
#[test]
fn a_failed_prepare_keeps_the_reason() {
    let mut session = Session::default();
    session
        .begin(
            "op".to_owned(),
            device("pc1"),
            Some(plan()),
            NOW,
        )
        .expect("第一次迁移该开得起来");

    let effects = session.on_ack(
        &device("pc1"),
        &failed("op", "第 3 版里没有条目 12"),
        NOW + 100,
    );

    assert_eq!(
        effects,
        vec![Effect::Abort {
            operation_id: "op".to_owned()
        }]
    );
    assert!(
        session
            .failure()
            .is_some_and(|why| why.contains("条目 12")),
        "{:?}",
        session.failure()
    );
}

/// 目标明确说开始失败了:作罢,输出仍是(已经停了的)源,**不自动恢复源**。
#[test]
fn a_failed_start_leaves_the_stopped_source_as_the_output()
 {
    let mut session = starting_local_to_pc1();

    let effects = session.on_ack(
        &device("pc1"),
        &failed("op", "没有准备这一次"),
        NOW + 300,
    );

    assert_eq!(
        effects,
        vec![Effect::Abort {
            operation_id: "op".to_owned()
        }]
    );
    assert_eq!(session.output(), &Output::Local);
    assert_eq!(session.moving(), None);
}

/// 源明确说停不了:撤掉目标的准备,作罢。
#[test]
fn a_source_that_cannot_stop_cancels_the_target() {
    let mut session = stopping_local_to_pc1();

    let effects = session.on_ack(
        &Output::Local,
        &failed("op", "播放器不可用"),
        NOW + 200,
    );

    assert_eq!(
        effects,
        vec![
            Effect::Cancel {
                operation_id: "op".to_owned(),
                to: device("pc1"),
            },
            Effect::Abort {
                operation_id: "op".to_owned()
            },
        ]
    );
}

/// 收回本机时进行中的那一次一并作废,输出回本机。
#[test]
fn coming_home_drops_the_move() {
    let mut session = stopping_local_to_pc1();

    session.come_home();

    assert_eq!(session.output(), &Output::Local);
    assert_eq!(session.moving(), None);
}

/// 服务端没登记下:这一次作废,原因记下来;别的操作的拒绝不理。
#[test]
fn a_move_the_server_rejects_is_dropped() {
    let mut session = Session::default();
    session
        .begin(
            "op".to_owned(),
            device("pc1"),
            Some(plan()),
            NOW,
        )
        .expect("第一次迁移该开得起来");

    session.rejected("older", "device_offline");
    assert!(
        session.moving().is_some(),
        "别的操作的拒绝不算"
    );

    session.rejected(
        "op",
        "device_offline: 设备 pc1 不在线",
    );
    assert_eq!(session.moving(), None);
    assert!(
        session
            .failure()
            .is_some_and(|why| why.contains("不在线"))
    );
    assert_eq!(session.output(), &Output::Local);
}

/// 按设备 id 认出迁移的两端 —— 带着会话里记的那个名字,回话才对得上。
#[test]
fn the_parties_of_a_move_are_found_by_device_id() {
    let mut session = session_at(device("a"));
    session
        .begin(
            "op".to_owned(),
            device("b"),
            Some(plan()),
            NOW,
        )
        .expect("迁移该开得起来");

    assert_eq!(session.party("a"), Some(device("a")));
    assert_eq!(session.party("b"), Some(device("b")));
    assert_eq!(session.party("c"), None);
    assert_eq!(
        Session::default().party("a"),
        None,
        "没在迁移就没有两端"
    );
}

/// 新的一次开始时,上一次的失败说明清掉。
#[test]
fn a_new_move_clears_the_last_failure() {
    let mut session = Session::default();
    session
        .begin(
            "op".to_owned(),
            device("pc1"),
            Some(plan()),
            NOW,
        )
        .expect("第一次迁移该开得起来");
    session.tick(NOW + PREPARE_TIMEOUT_MS + 1);
    assert!(session.failure().is_some());

    session
        .begin(
            "op2".to_owned(),
            device("pc1"),
            Some(plan()),
            NOW + 30_000,
        )
        .expect("失败之后可以再来");

    assert_eq!(session.failure(), None);
}
