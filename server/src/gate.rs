//! 请求进门那一道:token 换成账号的提取器,以及按账号计的限流。
//!
//! 只判「这条请求能不能往下走」,不认识任何业务。
//! 限流不只给 HTTP 用 —— 信令的握手也从这里取(见 [`crate::syncplay::signaling`])。

pub mod auth;
pub mod ratelimit;
