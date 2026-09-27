//! 每首歌的三态赞踩,与一次播放听了多久(#157)。

use serde::{Deserialize, Serialize};

/// `PUT /feedback/{track_id}` 的请求体。
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
)]
pub struct SetFeedbackDto {
    /// `1` 赞,`-1` 踩。取消赞踩改发 `DELETE`,不是把这个字段传成 0 ——
    /// 三态用"有没有这一行"表达,值域没有第三种取值。
    pub verdict: i16,
}

/// `GET /feedback/{track_id}` 的响应体:这首歌此刻的赞踩,没表态过是 `None`。
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
)]
pub struct FeedbackDto {
    pub verdict: Option<i16>,
}

/// `POST /played` 的响应体:这一行播放事件的 id。
///
/// 客户端拿它去 `PATCH /played/{id}/listened` 补听了多久 —— 起播与补记
/// 分两次请求,补记那次可能因为进程被杀而永远不发生,所以两者不能合成一次。
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
)]
pub struct PlayedAckDto {
    pub id: i64,
}

/// `PATCH /played/{id}/listened` 的请求体。
///
/// 只报原始数字,完播/跳过的口径由服务端查询时判定 —— 与事件流「口径想改就改」
/// 同一个理由(见服务端 `history` 模块)。
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
)]
pub struct ListenedDto {
    pub listened_ms: i64,
    pub duration_ms: i64,
}
