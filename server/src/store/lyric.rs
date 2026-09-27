//! 歌词标记的探测队列(#156)。
//!
//! 队列挂在 `platform_tracks` 的行上(见迁移 0017):`lyric_kind` 还是 `unknown`
//! 就在排队,写进缓存即入队。领取、租约、退避与 `prefetch` 同形。worker 的编排
//! 在 `routes::catalog::lyric_probe`。

use std::time::Duration;

use sqlx::PgConnection;

use crate::error::AppError;

/// 一首歌有没有歌词,各是 `lyric_kind` 列的一个取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyricKind {
    /// 平台没有歌词。
    None,
    /// 平台标了纯音乐。
    Instrumental,
    /// 有原文歌词。
    Lyric,
    /// 有歌词,还带翻译。
    Translated,
}

impl LyricKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Instrumental => "instrumental",
            Self::Lyric => "lyric",
            Self::Translated => "translated",
        }
    }
}

/// 一个被领走的探测任务。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Job {
    pub platform: String,
    pub track_id: String,
    /// 以谁的凭据去问平台。
    pub account_id: i64,
    /// 连这一次在内领过几次。
    pub attempts: i32,
}

/// 领这个平台的一首到点、还没探、没领满 `max_attempts` 次的歌,并把它推到
/// `lease` 之后。没有就是 `None`;库里一个账号都没有也是 `None`。
///
/// 凭据用绑过这个平台歌单的账号里 id 最小的那个,一个都没有就用任意账号 ——
/// 歌词不看账号权限,只是上游要一个身份才肯答。
pub async fn claim(
    conn: &mut PgConnection,
    platform: &str,
    lease: Duration,
    max_attempts: i32,
) -> Result<Option<Job>, AppError> {
    Ok(sqlx::query_as(
        "WITH asker AS (
             SELECT coalesce(
                 (SELECT min(account_id) FROM platform_playlist_tracks
                  WHERE platform = $2),
                 (SELECT min(id) FROM accounts)
             ) AS account_id
         )
         UPDATE platform_tracks
         SET lyric_probe_after = now() + $1::bigint * interval '1 second',
             lyric_attempts = lyric_attempts + 1
         FROM asker
         WHERE asker.account_id IS NOT NULL
           AND (platform, track_id) = (
             SELECT platform, track_id FROM platform_tracks
             WHERE platform = $2 AND lyric_kind = 'unknown'
               AND lyric_probe_after <= now() AND lyric_attempts < $3
             ORDER BY lyric_probe_after, track_id
             LIMIT 1
             FOR UPDATE SKIP LOCKED
         )
         RETURNING platform, track_id, asker.account_id, lyric_attempts AS attempts",
    )
    .bind(seconds(lease))
    .bind(platform)
    .bind(max_attempts)
    .fetch_optional(conn)
    .await?)
}

/// 探到了:记下标记,这首从此不在队列里。
pub async fn settle(
    conn: &mut PgConnection,
    job: &Job,
    kind: LyricKind,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE platform_tracks SET lyric_kind = $3
         WHERE platform = $1 AND track_id = $2",
    )
    .bind(&job.platform)
    .bind(&job.track_id)
    .bind(kind.as_str())
    .execute(conn)
    .await?;

    Ok(())
}

/// 这一次失败了:退避 `backoff × 次数` 之后再领。领满上限的由 [`claim`] 跳过。
pub async fn retry_later(
    conn: &mut PgConnection,
    job: &Job,
    backoff: Duration,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE platform_tracks
         SET lyric_probe_after =
             now() + $3::bigint * lyric_attempts * interval '1 second'
         WHERE platform = $1 AND track_id = $2",
    )
    .bind(&job.platform)
    .bind(&job.track_id)
    .bind(seconds(backoff))
    .execute(conn)
    .await?;

    Ok(())
}

fn seconds(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}
