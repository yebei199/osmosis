//! 设备之间的信令客户端:同账号的在线名册,以及遥控器模式(`docs/adr/0030`)。
//!
//! 与 `api`、`audio`、`render3d` 平行 —— `app-core` 不认识本 crate,由 `ui` 注入。
//!
//! 接上服务端的 `/signal`([`Signalling`]),由 [`Client`] 编排重连、接管与命令。
//! 同播(WebRTC 推流)已删(#137,`docs/adr/0008` 废止),crate 名是它留下的。

mod client;
pub mod clock;
mod session;
mod signalling;

pub use client::{Client, Event, SharedClock};
pub use session::Roster;
pub use signalling::{
    SignalSender, Signalling, command_wire_len,
    report_wire_len,
};

/// 设备的线上表示。
///
/// 从 `contract` 转出:界面层按分层不直接依赖契约 crate,而 [`Client::start`]
/// 又必须收一个设备身份 —— 由本层转达,调用方不必多引一个依赖。
pub use contract::DeviceDto;

/// 信令链路可能的失败方式。
#[derive(Debug)]
pub enum SyncError {
    /// 连不上信令服务器,或连接中途断了。
    Signalling(String),
    /// 服务端不认这个 token。
    ///
    /// 与 [`Self::Signalling`] 分开:那种失败该重试,这种不该 —— 换一个
    /// token 之前,再连也只是再得到一个 401。
    Unauthorized,
    /// 建连额度被限住了(429)。`retry_after` 是服务端说的秒数。
    ///
    /// 与 [`Self::Signalling`] 分开有两个用处。一是**退避照它的数来**:
    /// 服务端算得出还欠多少额度,客户端算不出,按自己的节奏重连只会把
    /// 闸撞得更死。二是**说得清**:用户看到的是「服务端限流,请等 N 秒」,
    /// 而不是一句 `HTTP error: 429`(#109 F-R3 现场那句横幅)。
    Throttled {
        retry_after: Option<core::time::Duration>,
    },
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
            Self::Unauthorized => {
                write!(f, "登录已失效")
            }
            Self::Throttled { retry_after } => {
                match retry_after {
                    Some(wait) => write!(
                        f,
                        "服务端限流,请等 {} 秒再试",
                        wait.as_secs()
                    ),
                    None => {
                        write!(f, "服务端限流,稍后再试")
                    }
                }
            }
        }
    }
}

impl core::error::Error for SyncError {}
