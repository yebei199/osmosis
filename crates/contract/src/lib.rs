//! 契约:客户端与服务端之间在网络上传输的数据的**形状** ——
//! 请求体、响应体、错误码、协议版本号。
//!
//! 契约刻意不包含领域规则。"一个订单不能被取消两次"是领域规则,属于
//! `app-core` 或服务端;"取消请求携带一个订单 id 字段"才是契约。
//! 两侧各自维护自己的领域模型,只在这里相遇。见 `docs/adr/0001`。

use serde::{Deserialize, Serialize};

/// 协议版本。客户端与服务端就线上格式达成的约定的版本号。
///
/// 任何对本 crate 中类型的**不兼容**改动都必须让它加一:改字段名、删字段、
/// 改字段语义。新增可选字段是兼容的,不必加一。
pub const PROTOCOL_VERSION: u32 = 1;

/// `GET /health` 的响应体。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct HealthDto {
    /// 服务端自述的状态。目前恒为 `"ok"` —— 能返回就说明活着。
    pub status: String,
    /// 服务端所用的 [`PROTOCOL_VERSION`]。客户端据此判断双方是否说同一种话。
    pub protocol_version: u32,
}

/// 一首可播放的歌曲。
///
/// 只保留客户端此刻用得上的字段。上游(bang-dream)的 `Track` 还有音质规格、
/// 付费等级等等 —— 用到时再加,新增可选字段是兼容变更,不必动 [`PROTOCOL_VERSION`]。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct TrackDto {
    /// 歌曲所属的平台,如 `"netease"`。
    ///
    /// 只有一个平台时它恒为同一个值,看着多余 —— 但歌曲的身份**本来就是**
    /// `(平台, 平台内 id)`,少了它,接第二个平台时两边的 id 会静默撞车。
    pub platform: String,
    /// 平台内的歌曲 id。与 [`Self::platform`] 合起来才唯一。
    pub id: String,
    pub title: String,
    /// 别名或副标题,常见于日文原名。平台没给就是 `None`。
    pub alias: Option<String>,
    /// 歌手名。保持列表形态 —— 怎么拼接是显示问题,属于 UI。
    pub artists: Vec<String>,
    /// 封面图地址。平台没给就是 `None`,不用空串冒充。
    pub cover: Option<String>,
    /// 时长,毫秒。与上游同单位,不做换算。
    pub duration_ms: i64,
}

/// `GET /search` 的响应体。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct SearchDto {
    pub tracks: Vec<TrackDto>,
    /// 还有没有下一页。翻页由客户端持有 offset 自行推进。
    pub has_more: bool,
}

/// `GET /play/{track_id}` 的响应体:一次取到的可播放源。
///
/// 这些字段属于"这次取到的源"而非歌曲本身 —— 换个音质档位再取会得到不同的值。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct PlaySourceDto {
    /// 平台签发的临时直链。带签名、会过期,**不要缓存**。
    pub url: String,
    /// 音频容器格式,如 `"mp3"`、`"flac"`。
    pub format: String,
    /// 码率,bit/s。
    pub bit_rate: i32,
    /// 是否只是试听片段(通常 30 秒)。
    ///
    /// 必须送到客户端:不告诉用户的话,一首歌放到 30 秒就停会被当成播放器坏了。
    pub trial: bool,
}

/// 请求失败时的响应体。
///
/// HTTP 状态码只做粗分类(4xx 请求方的问题 / 5xx 服务端这边的问题),
/// 具体语义由 [`Self::code`] 承担 —— 客户端按 `code` 分支,不按状态码。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct ErrorDto {
    /// 稳定的机读错误码,如 `"netease_not_logged_in"`。
    ///
    /// 它是契约的一部分:改动一个已有的 code 等于改字段语义,要动
    /// [`PROTOCOL_VERSION`]。新增 code 是兼容的。
    pub code: String,
    /// 给人看的说明。客户端不应该拿它做分支判断。
    pub message: String,
}
