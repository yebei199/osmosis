//! 播放事件与最近播放的集成测试,对着真实 Postgres 跑。
//!
//! 每个测试在自己的事务里跑完就回滚。起库见 `just pg`。

use server::account::{Account, register};
use server::error::AppError;
use server::playlist::TrackRef;
use server::{db, history};
use sqlx::{PgPool, Postgres, Transaction};

/// 与 `main.rs` 的默认值一致。
const DEFAULT_DATABASE_URL: &str =
    "postgres://slint:devonly@127.0.0.1:5432/osmosis";

const INVITE: &str = "let-me-in";

/// 取够多的条数,让「limit 生效」之外的测试不受它影响。
const PLENTY: i64 = 100;

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(
        |_| DEFAULT_DATABASE_URL.to_owned(),
    );

    db::connect(&url).await.unwrap_or_else(|err| {
        panic!(
            "连不上数据库({url}): {err}\n\
             起一个:just pg"
        )
    })
}

async fn tx() -> Transaction<'static, Postgres> {
    pool().await.begin().await.expect("开事务失败")
}

async fn make_account(
    tx: &mut Transaction<'static, Postgres>,
    username: &str,
) -> Account {
    register(tx, username, "correct horse", INVITE, INVITE)
        .await
        .expect("注册应该成功")
}

fn track(id: &str) -> TrackRef {
    TrackRef {
        platform: "netease".to_owned(),
        track_id: id.to_owned(),
    }
}

/// 记一次起播,能在最近播放里看到。
#[tokio::test]
async fn record_then_recent_returns_it() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "hs_basic").await;

    history::record(&mut tx, account.id, &track("1"))
        .await
        .expect("记录应该成功");

    let recent =
        history::recent(&mut tx, account.id, PLENTY)
            .await
            .expect("查询应该成功");

    assert_eq!(recent, vec![track("1")]);
}

/// 别人的播放事件不出现在我的最近播放里。
#[tokio::test]
async fn recent_is_scoped_to_the_account() {
    let mut tx = tx().await;
    let mine = make_account(&mut tx, "hs_scope_a").await;
    let theirs = make_account(&mut tx, "hs_scope_b").await;

    history::record(&mut tx, mine.id, &track("mine"))
        .await
        .unwrap();
    history::record(&mut tx, theirs.id, &track("theirs"))
        .await
        .unwrap();

    let recent = history::recent(&mut tx, mine.id, PLENTY)
        .await
        .unwrap();

    assert_eq!(recent, vec![track("mine")]);
}

/// 最近的在最前。
#[tokio::test]
async fn recent_lists_most_recent_first() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "hs_order").await;

    for id in ["1", "2", "3"] {
        history::record(&mut tx, account.id, &track(id))
            .await
            .unwrap();
    }

    let recent =
        history::recent(&mut tx, account.id, PLENTY)
            .await
            .unwrap();

    assert_eq!(
        recent,
        vec![track("3"), track("2"), track("1")]
    );
}

/// 同一首连听五遍,最近播放里只出现一次,且在它**最后一次**播放的位置上。
/// 不去重的话,一次单曲循环就能把整张列表填满。
#[tokio::test]
async fn repeated_plays_appear_once_at_their_latest_position()
 {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "hs_repeat").await;

    history::record(&mut tx, account.id, &track("old"))
        .await
        .unwrap();
    for _ in 0..5 {
        history::record(
            &mut tx,
            account.id,
            &track("looped"),
        )
        .await
        .unwrap();
    }
    history::record(&mut tx, account.id, &track("newest"))
        .await
        .unwrap();

    let recent =
        history::recent(&mut tx, account.id, PLENTY)
            .await
            .unwrap();

    assert_eq!(
        recent,
        vec![
            track("newest"),
            track("looped"),
            track("old")
        ]
    );
}

/// 去重只发生在**查询时**:事件流里那五条一条不少。
/// 统计口径将来要改(比如只算听完的),原始事件都在才改得动。
#[tokio::test]
async fn the_ledger_keeps_every_play() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "hs_ledger").await;

    for _ in 0..5 {
        history::record(
            &mut tx,
            account.id,
            &track("looped"),
        )
        .await
        .unwrap();
    }

    let rows: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM play_events WHERE account_id = $1",
    )
    .bind(account.id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();

    assert_eq!(rows.0, 5);
    assert_eq!(
        history::recent(&mut tx, account.id, PLENTY)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// limit 生效,而且数的是去重**之后**的条数 ——
/// 数去重之前的话,一次单曲循环会让 limit 20 只给出一首歌。
#[tokio::test]
async fn recent_respects_the_limit() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "hs_limit").await;

    // 第一首连听三遍,再听两首别的
    for _ in 0..3 {
        history::record(&mut tx, account.id, &track("a"))
            .await
            .unwrap();
    }
    for id in ["b", "c"] {
        history::record(&mut tx, account.id, &track(id))
            .await
            .unwrap();
    }

    let recent = history::recent(&mut tx, account.id, 2)
        .await
        .unwrap();

    assert_eq!(recent, vec![track("c"), track("b")]);
}

/// 记不存在的账号会失败而不是静默丢掉 —— 外键在守着这件事。
#[tokio::test]
async fn recording_for_an_unknown_account_fails() {
    let mut tx = tx().await;

    let result =
        history::record(&mut tx, -1, &track("1")).await;

    assert!(matches!(result, Err(AppError::Db(_))));
}
