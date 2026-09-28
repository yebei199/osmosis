//! 每首歌的三态赞踩(#157):独立于红心,红心管收藏,这里管评价。

use sqlx::PgConnection;

use crate::error::AppError;
use crate::store::playlist::TrackRef;

/// 点赞或点踩:一账号一曲目至多一行,再点已有的那一档就覆盖成新值。
pub async fn set(
    conn: &mut PgConnection,
    account_id: i64,
    track: &TrackRef,
    verdict: i16,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO track_feedback (account_id, platform, track_id, verdict)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (account_id, platform, track_id)
         DO UPDATE SET verdict = EXCLUDED.verdict, updated_at = now()",
    )
    .bind(account_id)
    .bind(&track.platform)
    .bind(&track.track_id)
    .bind(verdict)
    .execute(conn)
    .await?;

    Ok(())
}

/// 取消赞踩:再点已生效的那一档时,客户端改发这个。没有行也不报错 ——
/// 两次的意图是同一个(与 [`crate::store::liked::set`] 同一个理由)。
pub async fn clear(
    conn: &mut PgConnection,
    account_id: i64,
    track: &TrackRef,
) -> Result<(), AppError> {
    sqlx::query(
        "DELETE FROM track_feedback
         WHERE account_id = $1 AND platform = $2 AND track_id = $3",
    )
    .bind(account_id)
    .bind(&track.platform)
    .bind(&track.track_id)
    .execute(conn)
    .await?;

    Ok(())
}

/// 这首歌此刻的赞踩,没表态过是 `None`。播放页据此画按钮的高亮态。
pub async fn get(
    conn: &mut PgConnection,
    account_id: i64,
    track: &TrackRef,
) -> Result<Option<i16>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT verdict FROM track_feedback
         WHERE account_id = $1 AND platform = $2 AND track_id = $3",
    )
    .bind(account_id)
    .bind(&track.platform)
    .bind(&track.track_id)
    .fetch_optional(conn)
    .await?)
}
