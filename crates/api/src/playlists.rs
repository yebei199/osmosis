//! 歌单的读写,以及红心与订阅的开关。

use contract::{
    PlaylistDto, PlaylistsDto, TrackIdsDto, TracksDto,
};
use serde::Serialize;

use crate::url::{
    liked_url, platform_playlist_tracks_url,
    playlist_tracks_url, playlist_url, subscription_url,
};
use crate::{ApiError, base_url, platform};

/// `GET /playlists` —— 两个来源合并后的歌单列表,「我喜欢的」在最前。
pub async fn playlists() -> Result<PlaylistsDto, ApiError> {
    platform::get_json(format!("{}/playlists", base_url()))
        .await
}

/// `POST /playlists` —— 建一个本地歌单。
pub async fn create_playlist(
    name: &str,
) -> Result<PlaylistDto, ApiError> {
    platform::send_json(
        reqwest::Method::POST,
        format!("{}/playlists", base_url()),
        Some(Named {
            name: name.to_owned(),
        }),
    )
    .await
}

/// `PATCH /playlists/{id}` —— 给本地歌单改名。
pub async fn rename_playlist(
    id: &str,
    name: &str,
) -> Result<(), ApiError> {
    platform::send_no_content(
        reqwest::Method::PATCH,
        playlist_url(id),
        Some(Named {
            name: name.to_owned(),
        }),
    )
    .await
}

/// `DELETE /playlists/{id}` —— 删掉本地歌单。
pub async fn delete_playlist(
    id: &str,
) -> Result<(), ApiError> {
    platform::send_no_content::<()>(
        reqwest::Method::DELETE,
        playlist_url(id),
        None,
    )
    .await
}

/// `GET /playlists/local/{id}/tracks` —— 本地歌单的曲目。
pub async fn playlist_tracks(
    id: &str,
) -> Result<TracksDto, ApiError> {
    platform::get_json(playlist_tracks_url(id)).await
}

/// `GET /playlists/platform/{id}/tracks` —— 平台歌单的曲目。
///
/// 与本地那条是两个函数而不是一个带来源参数的:调用方在点开一个歌单时
/// 就已经知道它是哪一种(列表里的 `source` 就是),合成一个只会让每个
/// 调用点先去问一遍。
pub async fn platform_playlist_tracks(
    id: &str,
) -> Result<TracksDto, ApiError> {
    platform::get_json(platform_playlist_tracks_url(id))
        .await
}

/// `POST /playlists/{id}/tracks` —— 往本地歌单加曲目。
pub async fn add_playlist_tracks(
    id: &str,
    tracks: &[(String, String)],
) -> Result<(), ApiError> {
    platform::send_no_content(
        reqwest::Method::POST,
        playlist_tracks_url(id),
        Some(TrackRefs::from(tracks)),
    )
    .await
}

/// `DELETE /playlists/{id}/tracks` —— 从本地歌单移掉曲目。
pub async fn remove_playlist_tracks(
    id: &str,
    tracks: &[(String, String)],
) -> Result<(), ApiError> {
    platform::send_no_content(
        reqwest::Method::DELETE,
        playlist_tracks_url(id),
        Some(TrackRefs::from(tracks)),
    )
    .await
}

/// `GET /liked/ids` —— 红心的全量标识。
///
/// 界面每一行都要问「这一首红心没有」,而 [`liked`] 给的是一页曲目,
/// 回答不了这个问题。取一次存成集合,之后本地标。
pub async fn liked_ids() -> Result<TrackIdsDto, ApiError> {
    platform::get_json(format!("{}/liked/ids", base_url()))
        .await
}

/// `PUT|DELETE /liked/{track_id}` —— 点红心或取消。
pub async fn set_liked(
    track_id: &str,
    liked: bool,
) -> Result<(), ApiError> {
    platform::send_no_content::<()>(
        toggle_method(liked),
        liked_url(track_id),
        None,
    )
    .await
}

/// `PUT|DELETE /subscriptions/playlists/{id}` —— 收藏平台歌单或取消。
pub async fn set_subscribed(
    playlist_id: &str,
    subscribed: bool,
) -> Result<(), ApiError> {
    platform::send_no_content::<()>(
        toggle_method(subscribed),
        subscription_url(playlist_id),
        None,
    )
    .await
}

/// 只有一个 `name` 字段的请求体,建歌单与改名共用。
#[derive(Serialize)]
struct Named {
    name: String,
}

/// 增删曲目的请求体。
#[derive(Serialize)]
struct TrackRefs {
    tracks: Vec<TrackRefDto>,
}

#[derive(Serialize)]
struct TrackRefDto {
    platform: String,
    id: String,
}

impl TrackRefs {
    fn from(tracks: &[(String, String)]) -> Self {
        Self {
            tracks: tracks
                .iter()
                .map(|(platform, id)| TrackRefDto {
                    platform: platform.clone(),
                    id: id.clone(),
                })
                .collect(),
        }
    }
}

/// 开与关只差一个方法名。写成一处,免得两个端点各写一遍再有一处写反。
fn toggle_method(on: bool) -> reqwest::Method {
    if on {
        reqwest::Method::PUT
    } else {
        reqwest::Method::DELETE
    }
}
