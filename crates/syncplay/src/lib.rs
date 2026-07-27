//! 同播能力层:让另一台设备听到本机正在放的声音。
//!
//! 与 `api`、`audio`、`render3d` 平行 —— `app-core` 不认识本 crate,由 `ui` 注入。
//!
//! 两件事:接上信令服务器([`Signalling`]),以及与另一台设备建立 WebRTC 连接([`Peer`])。
//! 音频**不经过服务器**:主控与听众之间是 P2P,服务器只负责让它们找到彼此
//! (`docs/adr/0008`)。
//!
//! 服务端把信令载荷当作不透明文本,所以载荷内部的结构由本层定义 —— 见 [`Envelope`]。
//! 这个分工是刻意的:改动 offer/answer/candidate 的编码方式不必动服务端一行。

mod envelope;
mod peer;
pub mod pump;
mod session;
mod signalling;

pub use envelope::Envelope;
pub use peer::{Peer, PeerRole};
pub use session::{Role, Roster, Session};
pub use signalling::Signalling;

/// 同播链路可能的失败方式。
#[derive(Debug)]
pub enum SyncError {
    /// 连不上信令服务器,或连接中途断了。
    Signalling(String),
    /// WebRTC 那一侧出错:建连、协商、加轨。
    Peer(String),
    /// 收到一段读不懂的载荷。
    Envelope(String),
}

impl core::fmt::Display for SyncError {
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter<'_>,
    ) -> core::fmt::Result {
        match self {
            Self::Signalling(message) => {
                write!(f, "信令错误: {message}")
            }
            Self::Peer(message) => {
                write!(f, "连接错误: {message}")
            }
            Self::Envelope(message) => {
                write!(f, "信令载荷错误: {message}")
            }
        }
    }
}

impl core::error::Error for SyncError {}
