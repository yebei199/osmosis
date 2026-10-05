//! 每个账号一份共享的电台歌单(#186):成员与加入次序。
//!
//! 详情不在这里,借 [`super::cache`];听没听过也不存,读的时候看 `play_events`
//! (与电台拉新歌时丢听过的同一个判据,#159)。听过的行留着,「已听过」要摆它们。

use sqlx::PgConnection;

use crate::error::AppError;
use crate::store::playlist::TrackRef;

/// 歌单里的一首。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub track: TrackRef,
    /// 这个账号播放过它。
    pub heard: bool,
}

/// 把 `tracks` 按给定次序追加到队尾。已经在里面的跳过。返回新加了几首。
pub async fn append(
    conn: &mut PgConnection,
    account_id: i64,
    tracks: &[TrackRef],
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
    let done = sqlx::query(
        "INSERT INTO radio_tracks (account_id, platform, track_id)
         SELECT $1, t.platform, t.track_id
         FROM unnest($2::text[], $3::text[])
              WITH ORDINALITY AS t (platform, track_id, n)
         ORDER BY t.n
         ON CONFLICT DO NOTHING",
    )
    .bind(account_id)
    .bind(platforms)
    .bind(ids)
    .execute(conn)
    .await?;

    Ok(done.rows_affected())
}

/// 整份歌单,先加的在前。
pub async fn list(
    conn: &mut PgConnection,
    account_id: i64,
) -> Result<Vec<Entry>, AppError> {
    let rows: Vec<(String, String, bool)> = sqlx::query_as(
        "SELECT r.platform, r.track_id, EXISTS (
             SELECT 1 FROM play_events AS pe
             WHERE pe.account_id = r.account_id
               AND pe.platform = r.platform
               AND pe.track_id = r.track_id
         )
         FROM radio_tracks AS r
         WHERE r.account_id = $1
         ORDER BY r.id",
    )
    .bind(account_id)
    .fetch_all(conn)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(platform, track_id, heard)| Entry {
            track: TrackRef { platform, track_id },
            heard,
        })
        .collect())
}

/// 这首在不在这个账号的电台歌单里。
pub async fn holds(
    conn: &mut PgConnection,
    account_id: i64,
    track: &TrackRef,
) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM radio_tracks
             WHERE account_id = $1 AND platform = $2 AND track_id = $3
         )",
    )
    .bind(account_id)
    .bind(&track.platform)
    .bind(&track.track_id)
    .fetch_one(conn)
    .await?)
}
