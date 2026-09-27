//! 自定义标签的线上格式(#158)。账号级:同一首歌在哪个歌单里标签都一样。

use serde::{Deserialize, Serialize};

/// 一个标签打上的来源。期 2 的音频模型会打风格/乐器标签(#162),
/// 落进同一套表,靠这个字段与手打的区分开。
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TagSource {
    Manual,
    Model,
}

/// 一个标签。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct TagDto {
    pub id: String,
    pub name: String,
    pub source: TagSource,
}

/// `GET /tags` 与 `GET /tracks/{platform}/{id}/tags` 的响应体。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct TagsDto {
    pub tags: Vec<TagDto>,
}
