//! 每首歌的三态赞踩(#157)。独立于红心 —— 红心管收藏,这里管评价。

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use contract::{FeedbackDto, SetFeedbackDto};

use server::error;
use server::error::Failure;
use server::store::account::Account;
use server::store::feedback;
use server::store::playlist::TrackRef;

use crate::routes::catalog::catalog_cache::netease_name;
use crate::{AppState, conn};

/// `GET /feedback/{track_id}` —— 这首歌此刻的赞踩,没表态过给 `None`。
pub(crate) async fn get_feedback(
    State(state): State<AppState>,
    account: Account,
    Path(track_id): Path<String>,
) -> Result<Json<FeedbackDto>, Failure> {
    let mut conn = conn(&state.pool).await?;
    let track = TrackRef {
        platform: netease_name(),
        track_id,
    };
    let verdict = feedback::get(&mut conn, account.id, &track)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(Json(FeedbackDto { verdict }))
}

/// `PUT /feedback/{track_id}` —— 点赞或点踩。再点已生效的那一档要改发
/// `DELETE`,这条只管"设成某个值"。
pub(crate) async fn set_feedback(
    State(state): State<AppState>,
    account: Account,
    Path(track_id): Path<String>,
    Json(body): Json<SetFeedbackDto>,
) -> Result<StatusCode, Failure> {
    if body.verdict != 1 && body.verdict != -1 {
        return Err(error::map_error(
            &server::error::AppError::Invalid(
                "verdict 只能是 1 或 -1",
            ),
        ));
    }

    let mut conn = conn(&state.pool).await?;
    let track = TrackRef {
        platform: netease_name(),
        track_id,
    };
    feedback::set(&mut conn, account.id, &track, body.verdict)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /feedback/{track_id}` —— 取消赞踩。
pub(crate) async fn clear_feedback(
    State(state): State<AppState>,
    account: Account,
    Path(track_id): Path<String>,
) -> Result<StatusCode, Failure> {
    let mut conn = conn(&state.pool).await?;
    let track = TrackRef {
        platform: netease_name(),
        track_id,
    };
    feedback::clear(&mut conn, account.id, &track)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests;
