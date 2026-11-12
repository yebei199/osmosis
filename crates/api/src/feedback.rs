//! 每首歌的赞踩(#157):踩、赞、特别喜欢(ADR 0042),独立于红心 —— 红心管收藏,
//! 这里管评价。

use contract::{FeedbackDto, SetFeedbackDto};

use crate::{ApiError, base_url, platform};

fn feedback_url(track_id: &str) -> String {
    format!("{}/feedback/{}", base_url(), track_id)
}

/// `GET /feedback/{track_id}` —— 这首歌此刻的赞踩,没表态过给 `None`。
pub async fn feedback(
    track_id: &str,
) -> Result<FeedbackDto, ApiError> {
    platform::get_json(feedback_url(track_id)).await
}

/// `PUT /feedback/{track_id}` —— 设成某一档(`-1` 踩、`1` 赞、`2` 特别喜欢)。
pub async fn set_feedback(
    track_id: &str,
    verdict: i16,
) -> Result<(), ApiError> {
    platform::send_no_content(
        reqwest::Method::PUT,
        feedback_url(track_id),
        Some(SetFeedbackDto { verdict }),
    )
    .await
}

/// `DELETE /feedback/{track_id}` —— 取消赞踩。
pub async fn clear_feedback(
    track_id: &str,
) -> Result<(), ApiError> {
    platform::send_no_content::<()>(
        reqwest::Method::DELETE,
        feedback_url(track_id),
        None,
    )
    .await
}
