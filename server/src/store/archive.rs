//! 存进对象存储的曲目:桶里有什么、什么时候该删(#126)。
//!
//! 与 [`cache`](super::cache) 同属缓存:整张删掉只是下一次重新找网易云要。
//! 字节在桶里,这里只记账 —— 对象的增删由 `routes::play::archive` 编排,
//! 这一层不认识 S3。

use std::time::Duration;

use sqlx::PgConnection;

use crate::error::AppError;
use crate::store::liked;

/// 一个存进桶里的对象。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct StoredTrack {
    pub platform: String,
    pub track_id: String,
    pub quality: String,
    pub object_key: String,
    pub format: String,
    pub bit_rate: i32,
    pub bytes: i64,
}

/// 按「曲目 + 档位」找那一行。
pub async fn find(
    conn: &mut PgConnection,
    platform: &str,
    track_id: &str,
    quality: &str,
) -> Result<Option<StoredTrack>, AppError> {
    Ok(sqlx::query_as(
        "SELECT platform, track_id, quality, object_key, format, bit_rate, bytes
         FROM stored_tracks
         WHERE platform = $1 AND track_id = $2 AND quality = $3",
    )
    .bind(platform)
    .bind(track_id)
    .bind(quality)
    .fetch_optional(conn)
    .await?)
}

/// 把「最后一次播放」拨到现在。返回这首存没存过 —— 存过就不必再下载一遍。
///
/// 播放与取消红心都走这里:取消红心那一刻起重新数三天,而不是按很久以前那次
/// 播放算、当场就删。
pub async fn touch(
    conn: &mut PgConnection,
    platform: &str,
    track_id: &str,
    quality: &str,
) -> Result<bool, AppError> {
    let done = sqlx::query(
        "UPDATE stored_tracks SET last_played_at = now()
         WHERE platform = $1 AND track_id = $2 AND quality = $3",
    )
    .bind(platform)
    .bind(track_id)
    .bind(quality)
    .execute(conn)
    .await?;

    Ok(done.rows_affected() > 0)
}

/// 对象进了桶之后记一笔。已有同一行就覆盖 —— 那是对象被重新存过一次。
pub async fn record(
    conn: &mut PgConnection,
    track: &StoredTrack,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO stored_tracks
             (platform, track_id, quality, object_key, format, bit_rate, bytes)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (platform, track_id, quality) DO UPDATE SET
             object_key = EXCLUDED.object_key,
             format = EXCLUDED.format,
             bit_rate = EXCLUDED.bit_rate,
             bytes = EXCLUDED.bytes,
             stored_at = now(),
             last_played_at = now()",
    )
    .bind(&track.platform)
    .bind(&track.track_id)
    .bind(&track.quality)
    .bind(&track.object_key)
    .bind(&track.format)
    .bind(track.bit_rate)
    .bind(track.bytes)
    .execute(conn)
    .await?;

    Ok(())
}

/// 删掉那一行,不问条件。对象已经不在桶里时用 —— 留着它,这首就永远不会重新存。
pub async fn forget(
    conn: &mut PgConnection,
    track: &StoredTrack,
) -> Result<(), AppError> {
    sqlx::query(
        "DELETE FROM stored_tracks
         WHERE platform = $1 AND track_id = $2 AND quality = $3",
    )
    .bind(&track.platform)
    .bind(&track.track_id)
    .bind(&track.quality)
    .execute(conn)
    .await?;

    Ok(())
}

/// 「过期」的判定,[`expired`] 与 [`forget_if_expired`] 共用同一句 ——
/// 两处各写一遍的话,挑出来的与真删的迟早是两拨。
///
/// 红心集合以自家的「我的喜欢」为准(`docs/adr/0033`),任何一个账号红心了就留着。
const EXPIRED: &str = "last_played_at < now() - $1::bigint * interval '1 second'
     AND NOT EXISTS (
         SELECT 1 FROM local_playlist_tracks AS liked
         JOIN local_playlists AS list ON list.id = liked.playlist_id
         WHERE list.system = $2
           AND liked.platform = stored_tracks.platform
           AND liked.track_id = stored_tracks.track_id
     )";

/// 秒数进 SQL。三天这种量级离 `i64` 的上限远得很,溢出只可能是调用方写错了。
fn seconds(retain: Duration) -> i64 {
    i64::try_from(retain.as_secs()).unwrap_or(i64::MAX)
}

/// 没人红心、且最后一次播放已经早于 `retain` 之前的那些。
pub async fn expired(
    conn: &mut PgConnection,
    retain: Duration,
) -> Result<Vec<StoredTrack>, AppError> {
    Ok(sqlx::query_as(&format!(
        "SELECT platform, track_id, quality, object_key, format, bit_rate, bytes
         FROM stored_tracks WHERE {EXPIRED}"
    ))
    .bind(seconds(retain))
    .bind(liked::SYSTEM)
    .fetch_all(conn)
    .await?)
}

/// 对象删掉之后删那一行 —— **再判一次**过期。
///
/// 挑出来到删之间这首可能刚被播过:那时行留着,下一次 `/play` 发现对象不在,
/// 自己把行删掉并退回网易云,之后的 `/played` 会重新存它。
pub async fn forget_if_expired(
    conn: &mut PgConnection,
    track: &StoredTrack,
    retain: Duration,
) -> Result<(), AppError> {
    sqlx::query(&format!(
        "DELETE FROM stored_tracks
         WHERE platform = $3 AND track_id = $4 AND quality = $5 AND {EXPIRED}"
    ))
    .bind(seconds(retain))
    .bind(liked::SYSTEM)
    .bind(&track.platform)
    .bind(&track.track_id)
    .bind(&track.quality)
    .execute(conn)
    .await?;

    Ok(())
}
