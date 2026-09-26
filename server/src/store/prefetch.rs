//! 预取队列:该以无损存进桶的曲目排在这里,等后台 worker 去取(#147)。
//!
//! 一首一行,主键是 `(平台, 曲目)`,重复入队只会撞在主键上(见迁移 0013)。
//! 取完就删行;留下的行要么在排队,要么是没办成的(给不出无损、超出上限、
//! 重试用尽),等下一次入队再试。worker 的编排在 `routes::play::prefetch`。

use std::time::Duration;

use sqlx::PgConnection;

use crate::error::AppError;
use crate::store::playlist::TrackRef;

/// 一个被领走的任务。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Job {
    pub platform: String,
    pub track_id: String,
    /// 以谁的音源凭据去取。
    pub account_id: i64,
    /// 连这一次在内领过几次。
    pub attempts: i32,
}

impl Job {
    pub fn track(&self) -> TrackRef {
        TrackRef {
            platform: self.platform.clone(),
            track_id: self.track_id.clone(),
        }
    }
}

/// 没办成的那几种,各自是 `state` 列的一个取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unfinished {
    /// 音源给不出无损(或只给试听)。
    NoLossless,
    /// 超出空间上限,没存。
    OverCap,
}

impl Unfinished {
    fn state(self) -> &'static str {
        match self {
            Self::NoLossless => "no_lossless",
            Self::OverCap => "over_cap",
        }
    }
}

/// 入队的共同尾巴:已在排队的不动(包括正被领着的),没办成的重新排上。
const REQUEUE: &str =
    "ON CONFLICT (platform, track_id) DO UPDATE
     SET state = 'queued', attempts = 0, run_after = now()
     WHERE prefetch_jobs.state <> 'queued'";

/// 已经按 `$1` 那一档存进桶的不必入队。
const NOT_STORED: &str = "NOT EXISTS (
         SELECT 1 FROM stored_tracks s
         WHERE s.platform = t.platform AND s.track_id = t.track_id
           AND s.quality = $1
     )";

/// 以这个账号的凭据把这些曲目排上。返回新排上(或重新排上)的条数。
pub async fn enqueue(
    conn: &mut PgConnection,
    account_id: i64,
    tracks: &[TrackRef],
    quality: &str,
) -> Result<u64, AppError> {
    let (platforms, ids): (Vec<&str>, Vec<&str>) = tracks
        .iter()
        .map(|track| {
            (
                track.platform.as_str(),
                track.track_id.as_str(),
            )
        })
        .unzip();

    Ok(sqlx::query(&format!(
        "INSERT INTO prefetch_jobs (platform, track_id, account_id)
         SELECT DISTINCT t.platform, t.track_id, $2::bigint
         FROM unnest($3::text[], $4::text[]) AS t (platform, track_id)
         WHERE {NOT_STORED}
         {REQUEUE}"
    ))
    .bind(quality)
    .bind(account_id)
    .bind(platforms)
    .bind(ids)
    .execute(conn)
    .await?
    .rows_affected())
}

/// 所有账号的所有本地歌单(含「我的喜欢」)里的曲目都排上。启动时跑一次。
///
/// 同一首在几个账号的歌单里,以其中 id 最小的那个账号的凭据去取。
pub async fn enqueue_all_playlists(
    conn: &mut PgConnection,
    quality: &str,
) -> Result<u64, AppError> {
    Ok(sqlx::query(&format!(
        "INSERT INTO prefetch_jobs (platform, track_id, account_id)
         SELECT DISTINCT ON (t.platform, t.track_id)
                t.platform, t.track_id, p.account_id
         FROM local_playlist_tracks t
         JOIN local_playlists p ON p.id = t.playlist_id
         WHERE {NOT_STORED}
         ORDER BY t.platform, t.track_id, p.account_id
         {REQUEUE}"
    ))
    .bind(quality)
    .execute(conn)
    .await?
    .rows_affected())
}

/// 领这个音源的一个到点的任务,并把它推到 `lease` 之后 —— 领的人半路死掉,
/// 租约一过别人能再领。没有到点的就是 `None`。
///
/// 按音源领:每个音源有自己的 worker 与限速。`SKIP LOCKED`:几个 worker 同时
/// 来领,各拿各的,不排队等同一行。
pub async fn claim(
    conn: &mut PgConnection,
    platform: &str,
    lease: Duration,
) -> Result<Option<Job>, AppError> {
    Ok(sqlx::query_as(
        "UPDATE prefetch_jobs
         SET run_after = now() + $1::bigint * interval '1 second',
             attempts = attempts + 1
         WHERE (platform, track_id) = (
             SELECT platform, track_id FROM prefetch_jobs
             WHERE platform = $2 AND state = 'queued' AND run_after <= now()
             ORDER BY enqueued_at, track_id
             LIMIT 1
             FOR UPDATE SKIP LOCKED
         )
         RETURNING platform, track_id, account_id, attempts",
    )
    .bind(seconds(lease))
    .bind(platform)
    .fetch_optional(conn)
    .await?)
}

/// 办成了:删掉这一行。
pub async fn finish(
    conn: &mut PgConnection,
    job: &Job,
) -> Result<(), AppError> {
    sqlx::query(
        "DELETE FROM prefetch_jobs
         WHERE platform = $1 AND track_id = $2",
    )
    .bind(&job.platform)
    .bind(&job.track_id)
    .execute(conn)
    .await?;

    Ok(())
}

/// 没办成、也不必重试:记下原因,留着给统计数,等下一次入队。
pub async fn settle(
    conn: &mut PgConnection,
    job: &Job,
    why: Unfinished,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE prefetch_jobs SET state = $3
         WHERE platform = $1 AND track_id = $2",
    )
    .bind(&job.platform)
    .bind(&job.track_id)
    .bind(why.state())
    .execute(conn)
    .await?;

    Ok(())
}

/// 这一次失败了:领过 `max_attempts` 次的记 failed,否则退避
/// `backoff × 次数` 之后再领。
pub async fn retry_later(
    conn: &mut PgConnection,
    job: &Job,
    backoff: Duration,
    max_attempts: i32,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE prefetch_jobs
         SET state = CASE WHEN attempts >= $3 THEN 'failed' ELSE 'queued' END,
             run_after = now() + $4::bigint * attempts * interval '1 second'
         WHERE platform = $1 AND track_id = $2",
    )
    .bind(&job.platform)
    .bind(&job.track_id)
    .bind(max_attempts)
    .bind(seconds(backoff))
    .execute(conn)
    .await?;

    Ok(())
}

/// 各个状态各有几条,如 `[("queued", 12), ("over_cap", 3)]`。
pub async fn counts(
    conn: &mut PgConnection,
) -> Result<Vec<(String, i64)>, AppError> {
    Ok(sqlx::query_as(
        "SELECT state, count(*) FROM prefetch_jobs
         GROUP BY state ORDER BY state",
    )
    .fetch_all(conn)
    .await?)
}

fn seconds(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}
