//! 账号级自定义标签的读与写(#158)。

use contract::{TagDto, TagsDto};
use serde::Serialize;

use crate::url::{
    tag_track_url, tag_url, tags_url, track_tags_url,
};
use crate::{ApiError, platform};

/// `GET /tags` —— 这个账号的全部标签。
pub async fn tags() -> Result<TagsDto, ApiError> {
    platform::get_json(tags_url()).await
}

/// `POST /tags` —— 建一个标签(同名的直接复用)。
pub async fn create_tag(
    name: &str,
) -> Result<TagDto, ApiError> {
    platform::send_json(
        reqwest::Method::POST,
        tags_url(),
        Some(Named {
            name: name.to_owned(),
        }),
    )
    .await
}

/// `PATCH /tags/{id}` —— 给标签改名。
pub async fn rename_tag(
    id: &str,
    name: &str,
) -> Result<(), ApiError> {
    platform::send_no_content(
        reqwest::Method::PATCH,
        tag_url(id),
        Some(Named {
            name: name.to_owned(),
        }),
    )
    .await
}

/// `DELETE /tags/{id}` —— 删掉标签。
pub async fn delete_tag(id: &str) -> Result<(), ApiError> {
    platform::send_no_content::<()>(
        reqwest::Method::DELETE,
        tag_url(id),
        None,
    )
    .await
}

/// `GET /tracks/{platform}/{id}/tags` —— 一首歌打了哪些标签。
pub async fn track_tags(
    platform: &str,
    track_id: &str,
) -> Result<TagsDto, ApiError> {
    crate::platform::get_json(track_tags_url(
        platform, track_id,
    ))
    .await
}

/// `PUT|DELETE /tags/{id}/tracks/{platform}/{id}` —— 给一首歌打上/摘掉某个标签。
pub async fn set_track_tag(
    tag_id: &str,
    platform: &str,
    track_id: &str,
    on: bool,
) -> Result<(), ApiError> {
    let method = if on {
        reqwest::Method::PUT
    } else {
        reqwest::Method::DELETE
    };
    crate::platform::send_no_content::<()>(
        method,
        tag_track_url(tag_id, platform, track_id),
        None,
    )
    .await
}

/// 只有一个 `name` 字段的请求体,建标签与改名共用。
#[derive(Serialize)]
struct Named {
    name: String,
}
