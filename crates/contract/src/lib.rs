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
