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
    /// 向音源要的档位,键的一部分。
    pub quality: String,
    pub object_key: String,
    pub format: String,
    pub bit_rate: i32,
    pub bytes: i64,
    /// 音源实际给的档位(#147)。
    pub tier: String,
    pub bits_per_sample: Option<i32>,
    pub sample_rate: Option<i32>,
}

/// [`StoredTrack`] 的列,几条 SELECT 共用。
const COLUMNS: &str = "platform, track_id, quality, object_key, format, bit_rate, bytes,
     tier, bits_per_sample, sample_rate";

/// 按「曲目 + 档位」找那一行。
pub async fn find(
    conn: &mut PgConnection,
    platform: &str,
    track_id: &str,
    quality: &str,
) -> Result<Option<StoredTrack>, AppError> {
    Ok(sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM stored_tracks
         WHERE platform = $1 AND track_id = $2 AND quality = $3"
    ))
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
             (platform, track_id, quality, object_key, format, bit_rate, bytes,
              tier, bits_per_sample, sample_rate)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (platform, track_id, quality) DO UPDATE SET
             object_key = EXCLUDED.object_key,
             format = EXCLUDED.format,
             bit_rate = EXCLUDED.bit_rate,
             bytes = EXCLUDED.bytes,
             tier = EXCLUDED.tier,
             bits_per_sample = EXCLUDED.bits_per_sample,
             sample_rate = EXCLUDED.sample_rate,
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
    .bind(&track.tier)
    .bind(track.bits_per_sample)
    .bind(track.sample_rate)
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

/// 一首曲目在空间上限取舍里的名次,越小越先留(#147):「我的喜欢」0、当天日推 1、
/// 其他歌单 2、哪都不在 [`UNKEPT`]。任何一个账号的都算。
///
/// `platform`、`track_id` 是 SQL 表达式(列名或参数),不是值。
fn rank_sql(platform: &str, track_id: &str) -> String {
    format!(
        "CASE
         WHEN EXISTS (
             SELECT 1 FROM local_playlist_tracks lt
             JOIN local_playlists lp ON lp.id = lt.playlist_id
             WHERE lp.system = '{liked}'
               AND lt.platform = {platform} AND lt.track_id = {track_id}
         ) THEN 0
         WHEN EXISTS (
             SELECT 1 FROM daily_picks d
             WHERE d.platform = {platform} AND d.track_id = {track_id}
         ) THEN 1
         WHEN EXISTS (
             SELECT 1 FROM local_playlist_tracks lt
             WHERE lt.platform = {platform} AND lt.track_id = {track_id}
         ) THEN 2
         ELSE {UNKEPT} END",
        liked = liked::SYSTEM,
    )
}

/// 哪个歌单、哪份日推里都不在的名次。只有这一档按保留期清扫。
pub const UNKEPT: i32 = 3;

/// 「过期」的判定,[`expired`] 与 [`forget_if_expired`] 共用同一句 ——
/// 两处各写一遍的话,挑出来的与真删的迟早是两拨。
///
/// 不是按 `$2` 那一档要来的(#147 之前的 320k)一律过期。其余的:在我们任何
/// 一个歌单里、或在当天日推里的长期留着(#147);都不在的,最后一次播放早于
/// `$1` 秒之前就过期。
fn expired_sql() -> String {
    format!(
        "(quality <> $2 OR (
             {rank} = {UNKEPT}
             AND last_played_at < now() - $1::bigint * interval '1 second'
         ))",
        rank = rank_sql(
            "stored_tracks.platform",
            "stored_tracks.track_id"
        ),
    )
}

/// 秒数进 SQL。三天这种量级离 `i64` 的上限远得很,溢出只可能是调用方写错了。
fn seconds(retain: Duration) -> i64 {
    i64::try_from(retain.as_secs()).unwrap_or(i64::MAX)
}

/// 不是按 `quality` 那一档要来的,以及哪都不在、且最后一次播放已经早于
/// `retain` 之前的那些。
pub async fn expired(
    conn: &mut PgConnection,
    retain: Duration,
    quality: &str,
) -> Result<Vec<StoredTrack>, AppError> {
    Ok(sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM stored_tracks WHERE {}",
        expired_sql()
    ))
    .bind(seconds(retain))
    .bind(quality)
    .fetch_all(conn)
    .await?)
}

/// 对象删掉之后删那一行 —— **再判一次**过期。
///
/// 挑出来到删之间这首可能刚被播过:那时行留着,下一次 `/play` 发现对象不在,
/// 自己把行删掉并退回网易云,之后的 `/played` 会重新排它。
pub async fn forget_if_expired(
    conn: &mut PgConnection,
    track: &StoredTrack,
    retain: Duration,
    quality: &str,
) -> Result<(), AppError> {
    sqlx::query(&format!(
        "DELETE FROM stored_tracks
         WHERE platform = $3 AND track_id = $4 AND quality = $5 AND {}",
        expired_sql()
    ))
    .bind(seconds(retain))
    .bind(quality)
    .bind(&track.platform)
    .bind(&track.track_id)
    .bind(&track.quality)
    .execute(conn)
    .await?;

    Ok(())
}

/// 这首的名次(见 [`rank_sql`])。
pub async fn rank(
    conn: &mut PgConnection,
    platform: &str,
    track_id: &str,
) -> Result<i32, AppError> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT {}",
        rank_sql("$1", "$2")
    ))
    .bind(platform)
    .bind(track_id)
    .fetch_one(conn)
    .await?)
}

/// 名次在 `rank` 之后的存歌,先让位的在前:名次越靠后越先,同名次里最久没播的先。
pub async fn yielding_to(
    conn: &mut PgConnection,
    rank: i32,
) -> Result<Vec<StoredTrack>, AppError> {
    Ok(sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM (
             SELECT *, {rank_of} AS rank FROM stored_tracks
         ) AS ranked
         WHERE rank > $1
         ORDER BY rank DESC, last_played_at",
        rank_of = rank_sql(
            "stored_tracks.platform",
            "stored_tracks.track_id"
        ),
    ))
    .bind(rank)
    .fetch_all(conn)
    .await?)
}

/// 桶里一共占了多少字节,不分档位 —— 空间上限管的是整个桶。
pub async fn total_bytes(
    conn: &mut PgConnection,
) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT coalesce(sum(bytes), 0)::bigint FROM stored_tracks",
    )
    .fetch_one(conn)
    .await?)
}

/// 按 `quality` 那一档存了几首、占多少字节。
pub async fn usage(
    conn: &mut PgConnection,
    quality: &str,
) -> Result<(i64, i64), AppError> {
    Ok(sqlx::query_as(
        "SELECT count(*), coalesce(sum(bytes), 0)::bigint
         FROM stored_tracks WHERE quality = $1",
    )
    .bind(quality)
    .fetch_one(conn)
    .await?)
}
