//! Postgres 连接池与迁移。
//!
//! 这里存的是**自家的**数据:账号、本地歌单与「我的喜欢」、播放事件,以及平台
//! 曲目的缓存(`docs/adr/0018`、`docs/adr/0033`)。

use sqlx::postgres::{PgPool, PgPoolOptions};

/// 连接池上限。
///
// ponytail: 一个人用的服务,五条连接绰绰有余。真到并发瓶颈时再调。
const MAX_CONNECTIONS: u32 = 5;

/// 连上数据库并把迁移跑到最新。
///
/// 迁移在启动时跑而不是另开一条部署命令:少一个能忘的步骤,而代价只是启动慢几十毫秒。
/// 迁移失败就起不来 —— 带着旧 schema 服务比起不来更难查。
pub async fn connect(
    url: &str,
) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(MAX_CONNECTIONS)
        .connect(url)
        .await?;

    sqlx::migrate!().run(&pool).await?;

    Ok(pool)
}
