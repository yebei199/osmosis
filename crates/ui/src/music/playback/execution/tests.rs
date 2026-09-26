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
