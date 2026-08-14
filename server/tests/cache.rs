//! 平台曲目缓存的集成测试。
//!
//! 这里测的是「存得对、读得回」。刷新时机、以及界面什么时候看到新的那一份,
//! 是另一件事,断言的东西完全不同。
//!
//! 缓存不是镜像(见 `docs/adr/0018`):这些测试里没有一条断言「缓存里有而
//! 平台没有的东西要留着」—— 那正是它与镜像的分界线。

use contract::TrackDto;
use server::account::{Account, register};
use server::db;
use sqlx::{PgPool, Postgres, Transaction};

// 集成测试的 crate 根按**所在目录**找子模块,不按同名目录 —— 而 tests/ 下的
// 平级 .rs 每个都会被 cargo 编成独立的测试二进制。用 #[path] 指进子目录,
// 三组测试因此共用这一个二进制,也共用下面那几个夹具。
#[path = "cache/added_at.rs"]
mod added_at;
#[path = "cache/details.rs"]
mod details;
#[path = "cache/membership.rs"]
mod membership;

/// 与 `main.rs` 的默认值一致。
pub(crate) const DEFAULT_DATABASE_URL: &str =
    "postgres://slint:devonly@127.0.0.1:5432/osmosis";

pub(crate) const INVITE: &str = "let-me-in";

pub(crate) async fn pool() -> PgPool {
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

/// 一个用完即回滚的事务。测试之间因此互不可见,也不留数据。
pub(crate) async fn tx() -> Transaction<'static, Postgres> {
    pool().await.begin().await.expect("开事务失败")
}

/// 造一个账号。用户名带上测试名,免得并行时撞在唯一索引上。
pub(crate) async fn make_account(
    tx: &mut Transaction<'static, Postgres>,
    username: &str,
) -> Account {
    register(tx, username, "correct horse", INVITE, INVITE)
        .await
        .expect("注册应该成功")
}

pub(crate) fn track(id: &str, title: &str) -> TrackDto {
    TrackDto {
        platform: "netease".to_owned(),
        id: id.to_owned(),
        title: title.to_owned(),
        alias: None,
        artists: vec!["某人".to_owned()],
        cover: None,
        duration_ms: 200_000,
    }
}

/// 缓存里现有多少条曲目详情。直接查表:这几条测试要断言的正是
/// 「详情存了几份」,而那个数目没有任何对外接口会告诉你。
pub(crate) async fn detail_count(
    tx: &mut Transaction<'static, Postgres>,
) -> i64 {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM platform_tracks",
    )
    .fetch_one(&mut **tx)
    .await
    .expect("数详情条数失败");

    count
}
