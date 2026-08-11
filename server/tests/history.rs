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

/// 把一条播放事件塞到过去的某天。统计口径要多天数据,而 `record` 只会写 now()。
async fn record_days_ago(
    tx: &mut Transaction<'static, Postgres>,
    account_id: i64,
    track_id: &str,
    days: i32,
) {
    sqlx::query(
        "INSERT INTO play_events (account_id, platform, track_id, played_at)
         VALUES ($1, 'netease', $2, now() - $3 * INTERVAL '1 day')",
    )
    .bind(account_id)
    .bind(track_id)
    .bind(f64::from(days))
    .execute(&mut **tx)
    .await
    .expect("插入历史事件应该成功");
}

/// 统计全部是查询时聚合:本月起播数、听过的不同曲目数、连续在听天数。
#[tokio::test]
async fn stats_aggregate_the_ledger() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "hs_stats").await;

    for _ in 0..3 {
        history::record(&mut tx, account.id, &track("a"))
            .await
            .unwrap();
    }
    history::record(&mut tx, account.id, &track("b"))
        .await
        .unwrap();

    let stats = history::stats(&mut tx, account.id)
        .await
        .expect("统计应该成功");

    assert_eq!(stats.distinct_tracks, 2, "a 与 b 两首");
    assert_eq!(stats.streak_days, 1, "只有今天在听");
    // 月初跑这条测试时"本月"可能只包含今天,四条都该算进去。
    assert_eq!(stats.month_plays, 4);
}

/// 连续在听:从最近一个有播放的日子往回数,断一天即止;
/// 今天还没听不清零,到昨天为止仍算连着。
#[tokio::test]
async fn the_streak_counts_back_until_a_gap() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "hs_streak").await;

    // 昨天、前天在听,大前天断了,五天前又有一条
    record_days_ago(&mut tx, account.id, "x", 1).await;
    record_days_ago(&mut tx, account.id, "x", 2).await;
    record_days_ago(&mut tx, account.id, "x", 5).await;

    let stats =
        history::stats(&mut tx, account.id).await.unwrap();

    assert_eq!(
        stats.streak_days, 2,
        "昨天 + 前天连着,断在大前天"
    );
}

/// 断更超过一天,连续数归零 —— 上周听得再多也不算"在听"。
#[tokio::test]
async fn a_stale_streak_reads_as_zero() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "hs_stale").await;

    record_days_ago(&mut tx, account.id, "x", 3).await;
    record_days_ago(&mut tx, account.id, "x", 4).await;

    let stats =
        history::stats(&mut tx, account.id).await.unwrap();

    assert_eq!(stats.streak_days, 0);
}

/// 常听歌手:事件流连上曲目缓存,按出现次数排;缓存里没有详情的
/// 曲目不出现(缓存会随浏览补齐,统计跟着变准)。
#[tokio::test]
async fn top_artists_rank_by_plays() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "hs_artists").await;

    for (id, artists) in [
        ("a", &["独步"][..]),
        ("b", &["独步", "客串"][..]),
    ] {
        sqlx::query(
            "INSERT INTO platform_tracks
                 (platform, track_id, title, artists, duration_ms)
             VALUES ('netease', $1, $1, $2, 1000)",
        )
        .bind(id)
        .bind(artists)
        .execute(&mut *tx)
        .await
        .unwrap();
    }

    for _ in 0..3 {
        history::record(&mut tx, account.id, &track("a"))
            .await
            .unwrap();
    }
    history::record(&mut tx, account.id, &track("b"))
        .await
        .unwrap();
    // 缓存里没有的曲目:进事件流,不进榜单
    history::record(
        &mut tx,
        account.id,
        &track("uncached"),
    )
    .await
    .unwrap();

    let top = history::top_artists(&mut tx, account.id, 5)
        .await
        .expect("榜单应该成功");

    assert_eq!(
        top,
        vec![
            ("独步".to_owned(), 4),
            ("客串".to_owned(), 1)
        ]
    );
}

/// 记不存在的账号会失败而不是静默丢掉 —— 外键在守着这件事。
#[tokio::test]
async fn recording_for_an_unknown_account_fails() {
    let mut tx = tx().await;

    let result =
        history::record(&mut tx, -1, &track("1")).await;

    assert!(matches!(result, Err(AppError::Db(_))));
}
