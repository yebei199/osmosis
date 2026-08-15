//! 播放历史的上报与回读。

use contract::{PlayedDto, StatsDto, TracksDto};

use crate::{ApiError, base_url, platform};

/// `POST /played` —— 报告一次起播。
///
/// 在声音真的出来之后才调,不是按下播放键就调:取直链可能失败,
/// 那时并没有发生一次播放。
pub async fn record_play(
    platform_name: &str,
    track_id: &str,
) -> Result<(), ApiError> {
    platform::send_no_content(
        reqwest::Method::POST,
        format!("{}/played", base_url()),
        Some(PlayedDto {
            platform: platform_name.to_owned(),
            track_id: track_id.to_owned(),
        }),
    )
    .await
}

/// `GET /recent` —— 最近播放。
pub async fn recent() -> Result<TracksDto, ApiError> {
    platform::get_json(format!("{}/recent", base_url()))
        .await
}

/// `GET /stats` —— 收听统计,个人主页用。
pub async fn stats() -> Result<StatsDto, ApiError> {
    platform::get_json(format!("{}/stats", base_url()))
        .await
}
