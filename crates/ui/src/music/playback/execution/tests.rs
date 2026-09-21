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

// ---------------------------------------------------------------------------
// 取数那几秒里的竞态(AC-5、AC-8)
//
// 上面那些测的是账本本身,一条一条看都对;这一节测的是**它们在一次真的
// 取数前后按什么顺序被问到**。三条闸全都只在 `await` 之后才算数 —— 写在
// 前面的话每一条都仍然编得过、单测也仍然全绿,而错误只在「网络慢 + 用户
// 在这期间又动了一下」时出现,手工点是点不出来的。
//
// 取数用注入的闭包,不起真服务端:要模拟的东西正是「取数**期间**发生了
// 什么」,而那只能由测试自己在闭包里做。
//
// 判别力当场验过(2026-09-21),两次改坏各只红一条,没有一条是陪跑的:
//
// - 把 `controlled()` 挪到 `await` **之前**求值 →
//   `losing_control_during_the_fetch_keeps_the_copy_that_plays` 红,其余 21 条绿;
// - 把「还算不算数」那一闸挪到「成没成」**之后** →
//   `a_late_failure_writes_no_outcome_for_the_newer_tap` 红,其余 21 条绿。
// ---------------------------------------------------------------------------

use std::cell::Cell;

/// 把一个 future 跑到底。
///
// ponytail: 这里的 future 最多 pending 一次,不值得为它引一个运行时
fn block_on<F: core::future::Future>(
    future: F,
) -> F::Output {
    use core::task::{Context, Poll};

    let waker = core::task::Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut future = Box::pin(future);
    loop {
        if let Poll::Ready(value) =
            future.as_mut().poll(&mut cx)
        {
            return value;
        }
        std::thread::yield_now();
    }
}

fn entry(entry_id: i64, title: &str) -> api::QueueEntryDto {
    api::QueueEntryDto {
        entry_id,
        position: entry_id,
        track: app_core::TrackDto {
            platform: "netease".to_owned(),
            id: entry_id.to_string(),
            title: title.to_owned(),
            alias: None,
            artists: vec!["某人".to_owned()],
            cover: None,
            duration_ms: 1_000,
        },
    }
}

/// 一批取数结果,供闭包原样交回。
fn two_entries() -> Vec<api::QueueEntryDto> {
    vec![entry(11, "第一首"), entry(12, "第二首")]
}

/// 上面那一批换上之后该落进队列的曲目,**顺序一致**。
fn two_tracks() -> Vec<app_core::TrackDto> {
    two_entries()
        .into_iter()
        .map(|entry| entry.track)
        .collect()
}

/// 遥控器把同一条 `Play` 重发一遍:不重取、更不重新起播。
///
/// 重发是常态(命令丢了、重连之后补一次)。再取一遍、从头放那一首,在用户
/// 那里就是「歌自己跳回开头了」。判据放在取数**之前** —— 闭包一次都不该被叫到。
#[test]
fn a_repeat_of_an_applied_tap_never_even_fetches() {
    let execution = Execution::default();
    let calls = Cell::new(0);
    let fetch = || {
        calls.set(calls.get() + 1);
        core::future::ready(Ok::<_, String>(two_entries()))
    };

    let first = block_on(adopt_with(
        &execution,
        7,
        3,
        12,
        "op-A".to_owned(),
        || true,
        fetch,
    ));
    let again = block_on(adopt_with(
        &execution,
        7,
        3,
        12,
        "op-A".to_owned(),
        || true,
        fetch,
    ));

    assert_eq!(
        first,
        Adoption::Adopt {
            index: 1,
            tracks: two_tracks()
        }
    );
    assert_eq!(again, Adoption::AlreadyApplied);
    assert_eq!(
        calls.get(),
        1,
        "重发那一次不该再取一遍队列"
    );
}

/// 失败过的那一次**重试得了** —— 判据只差「应用过」这一位。
///
/// 分不开的话,一次网络抖动会把这首歌永久锁在「点不动」上。
#[test]
fn a_tap_that_failed_can_be_retried() {
    let execution = Execution::default();

    let failed = block_on(adopt_with(
        &execution,
        7,
        3,
        12,
        "op-A".to_owned(),
        || true,
        || core::future::ready(Err::<Vec<_>, _>("断网")),
    ));
    let retried = block_on(adopt_with(
        &execution,
        7,
        3,
        12,
        "op-A".to_owned(),
        || true,
        || {
            core::future::ready(Ok::<_, String>(
                two_entries(),
            ))
        },
    ));

    assert_eq!(failed, Adoption::Failed("断网".to_owned()));
    assert_eq!(
        retried,
        Adoption::Adopt {
            index: 1,
            tracks: two_tracks()
        }
    );
}

/// 取数期间遥控器又点了一首:迟到的那一份丢掉,不许把新的盖回去。
///
/// 盖回去的话用户看到的是「点了 B,放出来的是 A」。
#[test]
fn a_newer_tap_during_the_fetch_discards_the_late_one() {
    let execution = Execution::default();

    let late = block_on(adopt_with(
        &execution,
        7,
        3,
        11,
        "op-A".to_owned(),
        || true,
        || {
            // 取数这几秒里,B 发起了。
            execution.begin("op-B");
            execution.want(7, 4);
            core::future::ready(Ok::<_, String>(
                two_entries(),
            ))
        },
    ));

    assert_eq!(late, Adoption::Superseded);
    // A 那一份一个字都没写进账本:想要的仍是 B 要的那一版。
    assert_eq!(
        execution.identity(),
        (Some(7), Some(4), None)
    );
    assert_eq!(execution.take_outcome(), None);
}

/// 迟到的那一份**失败**了,也不许把下场记到新的那一次头上。
///
/// 记了的话,B 的那条报告会捎着 A 的失败原因出去,而遥控器据此把 B 标成
/// 没应用 —— 音箱里明明正放着 B。顺序因此是「先问还算不算数,再看成没成」。
#[test]
fn a_late_failure_writes_no_outcome_for_the_newer_tap() {
    let execution = Execution::default();

    let late = block_on(adopt_with(
        &execution,
        7,
        3,
        11,
        "op-A".to_owned(),
        || true,
        || {
            execution.begin("op-B");
            core::future::ready(Err::<Vec<_>, _>("超时"))
        },
    ));

    assert_eq!(late, Adoption::Superseded);
    assert_eq!(execution.take_outcome(), None);
    assert!(
        execution.still_current("op-B"),
        "B 还在途,不该被 A 的收尾顺手清掉"
    );
}

/// 取数期间失权 / 换目标 / 退出被控:作废在途那一次,**已经在放的那份不动**。
///
/// 失权不等于停止播放(`docs/adr/0030`:手机没电不能让 pc1 停)。
#[test]
fn losing_control_during_the_fetch_keeps_the_copy_that_plays()
 {
    let execution = Execution::default();
    execution.adopt(7, 3, vec![11, 12]);
    let controlled = Cell::new(true);

    let dropped = block_on(adopt_with(
        &execution,
        7,
        4,
        21,
        "op-B".to_owned(),
        || controlled.get(),
        || {
            controlled.set(false);
            core::future::ready(Ok::<_, String>(vec![
                entry(21, "新的那批"),
            ]))
        },
    ));

    assert_eq!(dropped, Adoption::Dropped);
    // 手上那份仍是第 3 版,条目号还在。
    assert_eq!(execution.entry_at(1), Some(12));
    assert_eq!(execution.identity().2, Some(3));
    assert!(!execution.still_current("op-B"));
}

/// 取不下来:保留旧副本,记一笔「没应用」,**不动 `applied_revision`**。
///
/// 它说的是「手上这份是哪一版」,而手上这份没换。
#[test]
fn a_failed_fetch_keeps_the_old_copy_and_reports_it() {
    let execution = Execution::default();
    execution.adopt(7, 3, vec![11, 12]);

    let failed = block_on(adopt_with(
        &execution,
        7,
        4,
        21,
        "op-B".to_owned(),
        || true,
        || core::future::ready(Err::<Vec<_>, _>("503")),
    ));

    assert_eq!(failed, Adoption::Failed("503".to_owned()));
    assert_eq!(execution.identity().2, Some(3));
    assert_eq!(
        execution.take_outcome(),
        Some(Outcome {
            operation_id: "op-B".to_owned(),
            applied: false,
            reason: Some("503".to_owned()),
        })
    );
}

/// 取回来的那一版里没有要播的那一条:别猜第一首。
///
/// 放一首没点过的歌比不出声更糟。
#[test]
fn an_entry_missing_from_that_revision_is_not_guessed() {
    let execution = Execution::default();

    let missing = block_on(adopt_with(
        &execution,
        7,
        3,
        99,
        "op-A".to_owned(),
        || true,
        || {
            core::future::ready(Ok::<_, String>(
                two_entries(),
            ))
        },
    ));

    assert_eq!(missing, Adoption::Missing);
    assert_eq!(execution.identity().2, None);
    assert_eq!(
        execution.take_outcome().map(|out| out.applied),
        Some(false)
    );
}

/// 顺利那一路:换上、对齐、记一笔应用成功。
#[test]
fn a_good_fetch_adopts_the_whole_revision() {
    let execution = Execution::default();

    let adopted = block_on(adopt_with(
        &execution,
        7,
        3,
        12,
        "op-A".to_owned(),
        || true,
        || {
            core::future::ready(Ok::<_, String>(
                two_entries(),
            ))
        },
    ));

    assert_eq!(
        adopted,
        Adoption::Adopt {
            index: 1,
            tracks: two_tracks()
        }
    );
    assert_eq!(
        execution.identity(),
        (Some(7), Some(3), Some(3))
    );
    assert_eq!(execution.entry_at(1), Some(12));
    assert_eq!(
        execution.take_outcome(),
        Some(Outcome {
            operation_id: "op-A".to_owned(),
            applied: true,
            reason: None,
        })
    );
}
