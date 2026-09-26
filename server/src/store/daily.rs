//! 每个账号最近一次取到的每日推荐(#147)。
//!
//! 只为存歌的保留规则记它(见 [`super::archive`]):日推的真相在平台,这里
//! 只是「最近一次取到的是哪几首」。

use sqlx::{Connection, PgConnection};

use crate::error::AppError;
use crate::store::playlist::TrackRef;

/// 把这个账号的日推整批换成 `tracks`。一个事务:换到一半失败不会只剩半批。
pub async fn replace(
    conn: &mut PgConnection,
    account_id: i64,
    tracks: &[TrackRef],
) -> Result<(), AppError> {
    let (platforms, ids): (Vec<&str>, Vec<&str>) = tracks
        .iter()
        .map(|track| {
            (
                track.platform.as_str(),
                track.track_id.as_str(),
            )
        })
        .unzip();

    let mut tx = conn.begin().await?;
    sqlx::query(
        "DELETE FROM daily_picks WHERE account_id = $1",
    )
    .bind(account_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO daily_picks (account_id, platform, track_id)
         SELECT $1, platform, track_id
         FROM unnest($2::text[], $3::text[]) AS t (platform, track_id)
         ON CONFLICT DO NOTHING",
    )
    .bind(account_id)
    .bind(platforms)
    .bind(ids)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(())
}
