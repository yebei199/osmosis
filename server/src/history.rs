//! 播放事件与最近播放。
//!
//! 事件流只增不改:起播即追加一条,不补记听了多久 —— 补记要靠客户端在退出或切歌时
//! 再发一次,而崩溃与断网时那一条就丢了(见 `docs/adr/0016`)。
//!
//! 「最近播放」「最常听」这类东西都是**查询时**的聚合,不存在独立的统计表。
//! 口径想改就改,因为原始事件都在。

use sqlx::PgConnection;

use crate::error::AppError;
use crate::playlist::TrackRef;

/// 记一次起播。
pub async fn record(
    conn: &mut PgConnection,
    account_id: i64,
    track: &TrackRef,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO play_events (account_id, platform, track_id)
         VALUES ($1, $2, $3)",
    )
    .bind(account_id)
    .bind(&track.platform)
    .bind(&track.track_id)
    .execute(conn)
    .await?;

    Ok(())
}

/// 最近播放的曲目标识,最近的在前。
///
/// **同一首歌只出现一次**,位置取它最后一次被播放的时刻。不去重的话,
/// 一次单曲循环就能把整张列表填满,而那张列表存在的意义正是"我最近都听了什么"。
///
/// 去重发生在这里而不是写入时:事件流那边一条不少,统计口径因此还改得动。
///
/// 排序用主键而不是 `played_at`:主键是严格递增的插入顺序,不依赖时钟 ——
/// 同一个事务里插入的几条 `now()` 完全相同,按时间排会得到任意顺序。
/// `played_at` 是数据,不是排序依据。
pub async fn recent(
    conn: &mut PgConnection,
    account_id: i64,
    limit: i64,
) -> Result<Vec<TrackRef>, AppError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT platform, track_id FROM (
             SELECT DISTINCT ON (platform, track_id)
                    platform, track_id, id
             FROM play_events
             WHERE account_id = $1
             ORDER BY platform, track_id, id DESC
         ) AS latest
         ORDER BY id DESC
         LIMIT $2",
    )
    .bind(account_id)
    .bind(limit)
    .fetch_all(conn)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(platform, track_id)| TrackRef {
            platform,
            track_id,
        })
        .collect())
}
