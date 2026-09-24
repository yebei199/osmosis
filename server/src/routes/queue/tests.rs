//! 队列六条路由的集成测试。
//!
//! 打真库、走真 handler,不用内存假货:这一层要说的话("超限回 413 而不是
//! 截断"、"旧版本号回 409"、"两次并发发布只有一个赢")全是数据库与 HTTP
//! 状态码的行为。
//!
//! 不用回滚事务:handler 自己从池里取连接,塞不进测试的事务里。与
//! `routes::testing` 同一条规矩 —— 固定账号名,开跑先删上一轮的残留,
//! 队列跟着账号级联走。

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use contract::{
    CreateQueueDto, PublishQueueDto, QueueIntentState,
    QueueOperationOutcomeDto, QueueReportDto,
    RemotePlayState, SetQueueIntentDto, TrackDto,
};
use similar_asserts::assert_eq;

use crate::routes::testing;

use super::{
    PageQuery, create_queue, publish_queue, queue_head,
    queue_page, report_queue_state, set_queue_intent,
};

/// 被控端那台。队列归属于播放会话 / 输出设备,不是账号。
const PC1: &str = "pc1";

fn track(id: &str) -> TrackDto {
    TrackDto {
        platform: "netease".to_owned(),
        id: id.to_owned(),
        title: format!("歌 {id}"),
        alias: None,
        artists: vec!["LiSA".to_owned()],
        cover: None,
        duration_ms: 234_000,
    }
}

/// 一份最小的执行报告。排列不带 —— 每秒那条本来就不带。
fn report(
    epoch: i64,
    state_seq: i64,
    applied_revision: i64,
) -> QueueReportDto {
    QueueReportDto {
        device_id: PC1.to_owned(),
        epoch,
        state_seq,
        applied_revision,
        entry_id: None,
        play_order: None,
        round: 0,
        position_ms: 0,
        state: RemotePlayState::Playing,
        operation: None,
    }
}

/// 一个只有库、没有上游的 state:队列这几条路由一个字节都不碰 bang-dream。
async fn state_for(
    case: &str,
) -> (crate::AppState, server::store::account::Account) {
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let state = testing::state(
        pool,
        testing::unreachable_upstream(),
    );
    (state, account)
}

/// 建出来的队列按上传顺序读得回来,版本从 1 起。
#[tokio::test]
async fn creating_a_queue_then_reading_it_back() {
    let (state, account) = state_for("q_http_create").await;

    let published = create_queue(
        State(state.clone()),
        account.clone(),
        Json(CreateQueueDto {
            device_id: PC1.to_owned(),
            tracks: vec![track("a"), track("b")],
        }),
    )
    .await
    .expect("建队列应该成功")
    .0;

    let page = queue_page(
        State(state),
        account,
        Path(published.queue_id),
        Query(PageQuery {
            revision: published.revision,
            offset: None,
            limit: None,
        }),
    )
    .await
    .expect("读队列应该成功")
    .0;

    assert_eq!(published.revision, 1);
    assert_eq!(page.total, 2);
    assert_eq!(page.offset, 0);
    let ids: Vec<&str> = page
        .entries
        .iter()
        .map(|row| row.track.id.as_str())
        .collect();
    assert_eq!(ids, vec!["a", "b"]);
}

/// 发布的应答直接带回这一版每一条的 `entry_id`,与之后读回来的一模一样 ——
/// 发布的那一端不必再分页把整份队列读回来找「我点的第 i 条是哪个条目」(#137 ③)。
///
/// 带一首重复的歌:条目号正是为了区分同一首歌出现的两处,拿曲目 id 猜会指错。
#[tokio::test]
async fn publishing_answers_with_the_entry_ids_of_that_revision()
 {
    let (state, account) =
        state_for("q_http_entry_ids").await;

    let created = create_queue(
        State(state.clone()),
        account.clone(),
        Json(CreateQueueDto {
            device_id: PC1.to_owned(),
            tracks: vec![
                track("a"),
                track("b"),
                track("a"),
            ],
        }),
    )
    .await
    .expect("建队列应该成功")
    .0;
    let republished = publish_queue(
        State(state.clone()),
        account.clone(),
        Path(created.queue_id),
        Json(PublishQueueDto {
            expected_revision: created.revision,
            tracks: vec![
                track("b"),
                track("c"),
                track("a"),
            ],
        }),
    )
    .await
    .expect("改队列应该成功")
    .0;

    for published in [&created, &republished] {
        let page = queue_page(
            State(state.clone()),
            account.clone(),
            Path(published.queue_id),
            Query(PageQuery {
                revision: published.revision,
                offset: None,
                limit: None,
            }),
        )
        .await
        .expect("读队列应该成功")
        .0;
        let read_back: Vec<i64> = page
            .entries
            .iter()
            .map(|row| row.entry_id)
            .collect();
        assert_eq!(
            published.entry_ids, read_back,
            "第 {} 版应答里的条目号与读回来的不一致",
            published.revision
        );
    }
}

/// 一页读不完时,`total` 说的是整版有多少条,不是这一页有多少条。
///
/// 少了它,客户端没法知道还要不要翻下一页 —— 而"按页数猜"在最后一页
/// 正好装满时会多要一页空的。
#[tokio::test]
async fn a_page_reports_the_total_beyond_its_own_length() {
    let (state, account) = state_for("q_http_page").await;

    let tracks: Vec<TrackDto> = (0..5)
        .map(|index| track(&index.to_string()))
        .collect();
    let published = create_queue(
        State(state.clone()),
        account.clone(),
        Json(CreateQueueDto {
            device_id: PC1.to_owned(),
            tracks,
        }),
    )
    .await
    .expect("建队列应该成功")
    .0;

    let page = queue_page(
        State(state),
        account,
        Path(published.queue_id),
        Query(PageQuery {
            revision: published.revision,
            offset: Some(3),
            limit: Some(2),
        }),
    )
    .await
    .expect("读队列应该成功")
    .0;

    assert_eq!(page.total, 5);
    assert_eq!(page.offset, 3);
    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.entries[0].track.id, "3");
}

/// 要一页比上限还大的,给到上限为止,不报错。
///
/// 报错的话客户端得先知道上限是多少才敢发第一次请求;夹住则是"要多少给多少,
/// 但不超过约定"——而 `total` 已经告诉它还剩几条。
#[tokio::test]
async fn a_limit_beyond_the_cap_is_clamped() {
    let (state, account) = state_for("q_http_clamp").await;

    let published = create_queue(
        State(state.clone()),
        account.clone(),
        Json(CreateQueueDto {
            device_id: PC1.to_owned(),
            tracks: vec![track("a")],
        }),
    )
    .await
    .expect("建队列应该成功")
    .0;

    let page = queue_page(
        State(state),
        account,
        Path(published.queue_id),
        Query(PageQuery {
            revision: published.revision,
            offset: None,
            limit: Some(9_999),
        }),
    )
    .await
    .expect("读队列应该成功")
    .0;

    assert!(
        page.entries.len() <= contract::QUEUE_PAGE_LIMIT,
        "一页最多 {} 条",
        contract::QUEUE_PAGE_LIMIT
    );
}

/// 超出约定规模明确拒绝:413 加一个客户端认得出的 code(AC-6)。
///
/// 不截断 —— 截了就是悄悄改掉用户点的那一批是什么,而界面上看不出来。
#[tokio::test]
async fn an_oversized_upload_answers_queue_too_large() {
    let (state, account) = state_for("q_http_quota").await;

    let tracks: Vec<TrackDto> = (0
        ..contract::MAX_QUEUE_ENTRIES + 1)
        .map(|index| track(&index.to_string()))
        .collect();

    let (status, body) = create_queue(
        State(state),
        account,
        Json(CreateQueueDto {
            device_id: PC1.to_owned(),
            tracks,
        }),
    )
    .await
    .expect_err("超限该被拒");

    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body.code, "queue_too_large");
}

/// 拿过期的版本号去改,回 409 与 `revision_conflict`(AC-8)。
///
/// 与 400 分开:请求本身没毛病,只是有人抢在前面改了,出路是重读一次当前
/// 版本再来 —— 客户端要按这个 code 分支,不按状态码。
#[tokio::test]
async fn publishing_against_a_stale_revision_answers_revision_conflict()
 {
    let (state, account) = state_for("q_http_stale").await;

    let first = create_queue(
        State(state.clone()),
        account.clone(),
        Json(CreateQueueDto {
            device_id: PC1.to_owned(),
            tracks: vec![track("a")],
        }),
    )
    .await
    .expect("建队列应该成功")
    .0;

    let _ = publish_queue(
        State(state.clone()),
        account.clone(),
        Path(first.queue_id),
        Json(PublishQueueDto {
            expected_revision: first.revision,
            tracks: vec![track("a"), track("b")],
        }),
    )
    .await
    .expect("第一次改应该成功")
    .0;

    let (status, body) = publish_queue(
        State(state),
        account,
        Path(first.queue_id),
        Json(PublishQueueDto {
            expected_revision: first.revision,
            tracks: vec![track("c")],
        }),
    )
    .await
    .expect_err("拿旧版本号去改该被拒");

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body.code, "revision_conflict");
}

/// 两次并发发布只有一个赢,而且 revision 2 里**只有赢家那一条**。
///
/// 这条测试的上一版没有判别力(#109 F-R2):它建队列时只放一首,两次并发
/// 各发一首**新**曲目,于是两边都拿到同一个 `entry_id = 2`,都要插入
/// `(queue_id, 2, 2)` —— `play_queue_entries` 的主键把其中一个挡了下来。
/// 无论 `FOR UPDATE` 有没有生效,它都看得到「只有一个赢」,分不清是锁挡的
/// 还是主键挡的。而当时实际生效的正是后者:路由那时用的是自动提交的连接,
/// 行锁在 `SELECT` 返回那一刻就还回去了(#109 F-R1)。
///
/// 这一版让两次发布**撞不上主键**:队列先放两首,两边各自沿用其中一首
/// 已有的 `entry_id`(走 carry-forward 分支),于是锁一旦失效,两边都能
/// 成功写进 revision 2 —— 断言这才红得起来。
///
/// 两个断言各管一半:「只有一个赢」管请求的回答,「revision 2 只有一条」
/// 管库里的事实。后者更硬 —— 即便时序碰巧让一方先跑完,两条都写进去了
/// 也逃不掉。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_concurrent_publishes_leave_exactly_one_winner()
{
    let (state, account) =
        state_for("q_http_concurrent").await;

    let first = create_queue(
        State(state.clone()),
        account.clone(),
        Json(CreateQueueDto {
            device_id: PC1.to_owned(),
            tracks: vec![track("a"), track("b")],
        }),
    )
    .await
    .expect("建队列应该成功")
    .0;

    // 第一次发布故意**大**:两次请求必须真的在时间上重叠,否则先发的那次
    // 早就跑完了,后发的那次读到的本来就是新版本 —— 那时无论有没有锁,
    // 看到的都是「一个赢」。
    //
    // 三千首在本机库上是几十毫秒,而下面那一下只等两毫秒 —— 第二次因此
    // 必定落在第一次的事务还开着的那段里。撤掉 `FOR UPDATE` 复验时,
    // 这个窗口决定 RED 出不出得来:八百首配十五毫秒试过,三次里有一次
    // 第一批已经跑完了,于是假绿。
    //
    // 两边的 entry_id 也不能撞:大的那批全是**新**曲目(拿 3 以后的号),
    // 小的那批沿用已有的 `a`(号 1)。撞上的话挡住第二次的会是
    // `play_queue_entries` 的主键而不是锁 —— 上一版正是栽在这里(#109 F-R2)。
    let bulk: Vec<TrackDto> = (0..3_000)
        .map(|index| track(&format!("new-{index}")))
        .collect();
    let left = tokio::spawn(publish_queue(
        State(state.clone()),
        account.clone(),
        Path(first.queue_id),
        Json(PublishQueueDto {
            expected_revision: first.revision,
            tracks: bulk,
        }),
    ));
    // 让大的那次先进到事务里去。没有这一下,两个任务可能一前一后跑完,
    // 而这条测试要验的恰恰是「重叠时会怎样」。
    tokio::time::sleep(std::time::Duration::from_millis(2))
        .await;
    let right = tokio::spawn(publish_queue(
        State(state.clone()),
        account.clone(),
        Path(first.queue_id),
        Json(PublishQueueDto {
            expected_revision: first.revision,
            tracks: vec![track("a")],
        }),
    ));
    let (left, right) = tokio::join!(left, right);
    let left = left.expect("发布任务不该 panic");
    let right = right.expect("发布任务不该 panic");

    let winners = [&left, &right]
        .iter()
        .filter(|outcome| outcome.is_ok())
        .count();
    assert_eq!(winners, 1, "两次并发发布只该有一个赢");

    let head = queue_head(
        State(state.clone()),
        account.clone(),
        Path(first.queue_id),
    )
    .await
    .expect("读队列头应该成功")
    .0;
    assert_eq!(head.revision, 2, "版本该正好推进一格");

    let page = queue_page(
        State(state),
        account,
        Path(first.queue_id),
        Query(PageQuery {
            revision: 2,
            offset: None,
            limit: None,
        }),
    )
    .await
    .expect("读 revision 2 应该成功")
    .0;
    assert_eq!(
        page.total,
        if left.is_ok() { 3_000 } else { 1 },
        "revision 2 里该只有赢家写的那一批 —— 两批都在,\
         说明两次发布都写进去了"
    );
}

/// 别的账号的队列一律 404,不是 403 —— 回 403 等于确认这个 id 存在。
#[tokio::test]
async fn another_accounts_queue_answers_not_found() {
    let (state, mine) = state_for("q_http_mine").await;
    let other =
        testing::fresh_account(&state.pool, "q_http_other")
            .await;

    let published = create_queue(
        State(state.clone()),
        mine,
        Json(CreateQueueDto {
            device_id: PC1.to_owned(),
            tracks: vec![track("a")],
        }),
    )
    .await
    .expect("建队列应该成功")
    .0;

    let (status, _) = queue_head(
        State(state),
        other,
        Path(published.queue_id),
    )
    .await
    .expect_err("别的账号不该读得到");

    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// 报告带着操作下场一起回来,那条意图就此标成已应用(AC-8)。
///
/// 同一条请求既说"我到哪了"又说"那次操作成了":分两次发会出现两者互相
/// 矛盾的中间态,而界面正是照着这两样决定要不要还挂着「待应用」。
#[tokio::test]
async fn a_report_marks_its_operation_applied() {
    let (state, account) =
        state_for("q_http_applied").await;

    let published = create_queue(
        State(state.clone()),
        account.clone(),
        Json(CreateQueueDto {
            device_id: PC1.to_owned(),
            tracks: vec![track("a")],
        }),
    )
    .await
    .expect("建队列应该成功")
    .0;
    let page = queue_page(
        State(state.clone()),
        account.clone(),
        Path(published.queue_id),
        Query(PageQuery {
            revision: published.revision,
            offset: None,
            limit: None,
        }),
    )
    .await
    .expect("读队列应该成功")
    .0;

    set_queue_intent(
        State(state.clone()),
        account.clone(),
        Path(published.queue_id),
        Json(SetQueueIntentDto {
            device_id: PC1.to_owned(),
            revision: published.revision,
            entry_id: page.entries[0].entry_id,
            operation_id: "op-1".to_owned(),
        }),
    )
    .await
    .expect("写意图应该成功");

    let mut done = report(200, 1, published.revision);
    done.operation = Some(QueueOperationOutcomeDto {
        operation_id: "op-1".to_owned(),
        applied: true,
        reason: None,
    });
    let ack = report_queue_state(
        State(state.clone()),
        account.clone(),
        Path(published.queue_id),
        Json(done),
    )
    .await
    .expect("写报告应该成功")
    .0;

    let head = queue_head(
        State(state),
        account,
        Path(published.queue_id),
    )
    .await
    .expect("读队列头应该成功")
    .0;

    assert!(ack.accepted);
    assert_eq!(
        head.intent.expect("该有一条意图").state,
        QueueIntentState::Applied
    );
}

/// 迟到的报告回 `accepted: false`,不回错误。
///
/// 乱序不是故障,是这条链路的常态;回 4xx 的话客户端要么重试(更乱)、
/// 要么打一条看起来像故障的日志,而它对此唯一该做的事是继续报下一条。
#[tokio::test]
async fn a_stale_report_is_answered_as_not_accepted() {
    let (state, account) =
        state_for("q_http_stale_report").await;

    let published = create_queue(
        State(state.clone()),
        account.clone(),
        Json(CreateQueueDto {
            device_id: PC1.to_owned(),
            tracks: vec![track("a")],
        }),
    )
    .await
    .expect("建队列应该成功")
    .0;

    let _ = report_queue_state(
        State(state.clone()),
        account.clone(),
        Path(published.queue_id),
        Json(report(200, 5, published.revision)),
    )
    .await
    .expect("写报告应该成功")
    .0;

    let ack = report_queue_state(
        State(state),
        account,
        Path(published.queue_id),
        Json(report(100, 99, published.revision)),
    )
    .await
    .expect("迟到的报告不该是错误")
    .0;

    assert!(!ack.accepted, "旧 epoch 的报告该被拒收");
}
