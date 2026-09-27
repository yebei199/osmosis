//! 每首歌三态赞踩的集成测试(#157),对着真实 Postgres 跑。
//!
//! 独立于「我的喜欢」:这张表不挂在本地歌单那两张表上,点赞踩不碰红心。

use server::store::account::{Account, register};
use server::store::db;
use server::store::feedback;
use server::store::playlist::TrackRef;
use sqlx::{PgPool, Postgres, Transaction};

const DEFAULT_DATABASE_URL: &str =
    "postgres://slint:devonly@127.0.0.1:5432/osmosis";

const INVITE: &str = "let-me-in";

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

/// 没表态过是 `None`。
#[tokio::test]
async fn untouched_track_reads_none() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "fb_none").await;

    let verdict =
        feedback::get(&mut tx, account.id, &track("1"))
            .await
            .expect("查询应该成功");

    assert_eq!(verdict, None);
}

/// 点赞再点踩:同一账号同一曲目只有一行,覆盖成新值,不是两行。
#[tokio::test]
async fn setting_again_overwrites_the_single_row() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "fb_overwrite").await;

    feedback::set(&mut tx, account.id, &track("1"), 1)
        .await
        .expect("点赞应该成功");
    feedback::set(&mut tx, account.id, &track("1"), -1)
        .await
        .expect("改点踩应该成功");

    let verdict =
        feedback::get(&mut tx, account.id, &track("1"))
            .await
            .expect("查询应该成功");
    assert_eq!(verdict, Some(-1));

    let (rows,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM track_feedback WHERE account_id = $1",
    )
    .bind(account.id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(rows, 1, "只有一行,不是两行");
}

/// 取消即删行,再取消一次(没有行)不报错。
#[tokio::test]
async fn clearing_removes_the_row_and_is_idempotent() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "fb_clear").await;

    feedback::set(&mut tx, account.id, &track("1"), 1)
        .await
        .unwrap();
    feedback::clear(&mut tx, account.id, &track("1"))
        .await
        .expect("取消应该成功");
    feedback::clear(&mut tx, account.id, &track("1"))
        .await
        .expect("再取消一次不该报错");

    let verdict =
        feedback::get(&mut tx, account.id, &track("1"))
            .await
            .unwrap();
    assert_eq!(verdict, None);
}

/// 别人的赞踩不出现在我这边。
#[tokio::test]
async fn feedback_is_scoped_to_the_account() {
    let mut tx = tx().await;
    let mine = make_account(&mut tx, "fb_scope_a").await;
    let theirs = make_account(&mut tx, "fb_scope_b").await;

    feedback::set(&mut tx, theirs.id, &track("1"), 1)
        .await
        .unwrap();

    let verdict =
        feedback::get(&mut tx, mine.id, &track("1"))
            .await
            .unwrap();
    assert_eq!(verdict, None);
}
