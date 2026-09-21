//! 执行副本那本账。
//!
//! 全是纯判断,与播放器、窗口、网络都无关 —— 而它们每一条写反了都只表现为
//! 「遥控器上显示的东西不太对」,从截图上看不出来。

use similar_asserts::assert_eq;

use super::*;

/// 应用一版之后,两个 revision 对齐 —— 也就是「不待应用」。
#[test]
fn adopting_a_revision_aligns_both_sides() {
    let execution = Execution::default();

    execution.adopt(7, 3, vec![11, 12]);

    assert_eq!(
        execution.identity(),
        (Some(7), Some(3), Some(3))
    );
}

/// 服务端有新版而还没应用:`desired` 走在前面,`applied` 留在原地。
///
/// 这一对正是界面上那句「新版本待应用」的来源。合成一个的话,下载失败时
/// 只能在「谎报已应用」与「谎报没收到」之间挑一个。
#[test]
fn wanting_a_newer_revision_leaves_applied_behind() {
    let execution = Execution::default();
    execution.adopt(7, 3, vec![11, 12]);

    execution.want(7, 4);

    assert_eq!(
        execution.identity(),
        (Some(7), Some(4), Some(3))
    );
}

/// 被要求换到**另一个队列**时,旧的对应关系一条都不作数。
///
/// 留着的话,新队列的第 0 条会顶着旧队列第 0 条的 `entry_id` 报出去 ——
/// 服务端那边看到的是一个它认得、但指着另一批歌的条目号。
#[test]
fn wanting_another_queue_drops_the_old_mapping() {
    let execution = Execution::default();
    execution.adopt(7, 3, vec![11, 12]);

    execution.want(8, 1);

    assert_eq!(
        execution.identity(),
        (Some(8), Some(1), None)
    );
    assert_eq!(execution.entry_at(0), None);
}

/// 条目号按位置查,越界给 `None`。
///
/// 不给 0:那会被读成「第一条」,而那是一句谎话 —— 上报里说错「正在放哪一条」
/// 之后,遥控器上高亮的就是另一首歌。
#[test]
fn entry_ids_are_looked_up_by_position() {
    let execution = Execution::default();
    execution.adopt(7, 3, vec![11, 12, 13]);

    assert_eq!(execution.entry_at(1), Some(12));
    assert_eq!(execution.entry_at(3), None);
}

/// 同一首歌在队列里出现两次,是**两个**条目。
///
/// 这一条是队列与歌单的分界线。按曲目去认的话两次出现会合成一个,
/// 于是用户点第二次出现的那一条,放的是第一次那一条。
#[test]
fn the_same_track_twice_keeps_two_entries() {
    let execution = Execution::default();
    execution.adopt(7, 3, vec![11, 12, 13]);

    assert_ne!(
        execution.entry_at(0),
        execution.entry_at(2)
    );
}

/// 操作下场只捎一次。
///
/// 留着的话,每秒那条报告会把同一次操作反复汇报,而服务端每收到一次就按
/// `operation_id` 改一次意图的状态。
#[test]
fn an_outcome_is_carried_exactly_once() {
    let execution = Execution::default();
    execution.note(Outcome {
        operation_id: "op-1".to_owned(),
        applied: true,
        reason: None,
    });

    let first = execution.take_outcome();
    let second = execution.take_outcome();

    assert_eq!(
        first.map(|outcome| outcome.operation_id),
        Some("op-1".to_owned())
    );
    assert_eq!(second, None, "同一次操作不该汇报两遍");
}

/// 本机自己攒的那一批:没有 `queue_id`,而那是正常状态。
#[test]
fn detaching_forgets_the_server_identity() {
    let execution = Execution::default();
    execution.adopt(7, 3, vec![11, 12]);

    execution.detach();

    assert_eq!(execution.identity(), (None, None, None));
}

/// 排列只在**变了**的时候才报一次。
///
/// 每秒都带的话,服务端那边就是每秒重写几千个 bigint,而线上字节数并不会涨
/// —— 一个 AC-2 抓不到的写放大。这一条钉的就是那道闸。
#[test]
fn an_unchanged_order_is_not_reported_again() {
    let execution = Execution::default();

    let first = execution.order_to_report(&[3, 1, 2]);
    let again = execution.order_to_report(&[3, 1, 2]);
    let shuffled = execution.order_to_report(&[2, 3, 1]);

    assert_eq!(first, Some(vec![3, 1, 2]), "第一次要报");
    assert_eq!(again, None, "没变就别再报一遍");
    assert_eq!(
        shuffled,
        Some(vec![2, 3, 1]),
        "洗过牌就要报新的那一份"
    );
}

/// 重试同一次点播**不再重置播放**(AC-5)。
///
/// 遥控器重发一遍是常态:命令丢了、重连之后补一次。重新取一遍队列、从头起播
/// 那一首,在用户那里就是「歌自己跳回开头了」。
#[test]
fn retrying_an_applied_operation_is_refused() {
    let execution = Execution::default();
    assert!(execution.begin("op-1"), "第一次该放行");
    execution.adopt(7, 3, vec![11]);
    execution.note(Outcome {
        operation_id: "op-1".to_owned(),
        applied: true,
        reason: None,
    });

    assert!(
        !execution.begin("op-1"),
        "同一次操作重试不该再动播放"
    );
}

/// 失败过的那一次,**重试是应该的**。
///
/// 与上一条正相反,而判据只差 `applied` 那一位:没成的操作重来一次正是
/// 用户要的。分不开的话,一次网络抖动会把这首歌永久锁在「点不动」上。
#[test]
fn retrying_a_failed_operation_is_allowed() {
    let execution = Execution::default();
    execution.begin("op-1");
    execution.note(Outcome {
        operation_id: "op-1".to_owned(),
        applied: false,
        reason: Some("断了".to_owned()),
    });

    assert!(
        execution.begin("op-1"),
        "没成的那一次该能重来"
    );
}

/// 连点 A、B:取数期间来了 B,**迟到的 A 不作数**(AC-5)。
#[test]
fn a_newer_operation_supersedes_the_one_in_flight() {
    let execution = Execution::default();
    execution.begin("op-a");

    execution.begin("op-b");

    assert!(
        !execution.still_current("op-a"),
        "A 已经被 B 顶掉了"
    );
    assert!(execution.still_current("op-b"));
}

/// 取数期间失权 / 换目标 / 退出被控:这一次作废(AC-5)。
///
/// 三种情形共用一个判据。作废的只是**在途那一次**,已经应用的那份副本
/// 一动不动 —— 失权不等于停止播放。
#[test]
fn abandoning_drops_the_operation_but_keeps_the_copy() {
    let execution = Execution::default();
    execution.adopt(7, 3, vec![11, 12]);
    execution.begin("op-1");

    execution.abandon();

    assert!(!execution.still_current("op-1"));
    assert_eq!(
        execution.identity(),
        (Some(7), Some(3), Some(3)),
        "已经在放的那一份不该被动"
    );
}

/// 同步不上去的那一批会**隔一阵再试**,不是每秒一发(AC-12)。
#[test]
fn an_unsynced_queue_retries_on_a_cadence() {
    let execution = Execution::default();

    assert!(
        execution.due_for_resync(100_000),
        "第一次该试"
    );
    assert!(
        !execution.due_for_resync(105_000),
        "五秒之后太密了"
    );
    assert!(
        execution.due_for_resync(140_000),
        "隔够了就再试一次"
    );
}

/// 已经有 `queue_id` 的那一批没什么可对的。
#[test]
fn a_synced_queue_is_never_due_for_resync() {
    let execution = Execution::default();
    execution.adopt(7, 3, vec![11]);

    assert!(!execution.due_for_resync(1_000_000));
}
