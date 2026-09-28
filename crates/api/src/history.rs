//! 播放历史的上报与回读。

use contract::{
    ListenedDto, PlayedAckDto, PlayedDto, StatsDto,
    TracksDto,
};

use crate::{ApiError, base_url, platform};

/// `POST /played` —— 报告一次起播,响应带这一行的 id。
///
/// 在声音真的出来之后才调,不是按下播放键就调:取直链可能失败,
/// 那时并没有发生一次播放。调用方拿着这个 id 去 [`report_listened`]
/// 补记这一次听了多久(#157)。
pub async fn record_play(
    platform_name: &str,
    track_id: &str,
) -> Result<PlayedAckDto, ApiError> {
    platform::send_json(
        reqwest::Method::POST,
        format!("{}/played", base_url()),
        Some(PlayedDto {
            platform: platform_name.to_owned(),
            track_id: track_id.to_owned(),
        }),
    )
    .await
}

/// `PATCH /played/{id}/listened` —— 补记这一次播放听了多久(#157)。
///
/// 切歌、播完、停止时各调一次;进程被杀时这条永远不会发生,接受
/// (见服务端 `history` 模块)。
pub async fn report_listened(
    play_event_id: i64,
    listened_ms: i64,
    duration_ms: i64,
) -> Result<(), ApiError> {
    platform::send_no_content(
        reqwest::Method::PATCH,
        format!(
            "{}/played/{play_event_id}/listened",
            base_url()
        ),
        Some(ListenedDto {
            listened_ms,
            duration_ms,
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
