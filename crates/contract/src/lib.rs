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
///
/// 2:音乐相关的路由开始要求登录态(既有路由多了一个必需的请求头,老客户端会
/// 整片 401),`/search` 拆成 `/search/tracks`、`/search/artists`、
mod account;
mod catalog;
mod playlist;
mod sync;

pub use account::*;
pub use catalog::*;
pub use playlist::*;
pub use sync::*;

/// 协议版本。客户端与服务端就线上格式达成的约定的版本号。
///
/// 任何对本 crate 中类型的**不兼容**改动都必须让它加一:改字段名、删字段、
/// 改字段语义。新增可选字段是兼容的,不必加一。
///
/// 2:音乐相关的路由开始要求登录态(既有路由多了一个必需的请求头,老客户端会
/// 整片 401),`/search` 拆成 `/search/tracks`、`/search/artists`、
/// `/search/playlists` 三条。
pub const PROTOCOL_VERSION: u32 = 2;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧版报文里没有新加的字段,不能因此整个解不出来。
    ///
    /// 服务端与客户端不是同时上线的:两边都能装着旧的那一半跑一阵。
    /// 少一个字段就整条响应失败的话,现象是「升级完 app 什么都拉不出来」,
    /// 而错误信息只会说"服务端的答复看不懂"。
    #[test]
    fn a_tracks_response_without_the_new_field_still_parses()
     {
        // 旧服务端发出来的那份:只有 tracks
        let dto: TracksDto =
            serde_json::from_str(r#"{"tracks":[]}"#)
                .expect("少一个字段不该让整条响应解不出来");

        assert_eq!(
            dto.unavailable, 0,
            "没提这件事就是一首都没少"
        );
    }
}
