//! 同播的线上格式:设备名册与双向信令。

use serde::{Deserialize, Serialize};

/// 一台在线设备。
///
/// 「在线」没有别的含义:它**等于**此刻与服务端之间存在活跃连接。
/// 服务端不记忆离线设备,所以名册里出现过就是现在能推流的(见 `docs/adr/0009`)。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct DeviceDto {
    /// 设备自己生成、本地保存的 id。服务端信任自报,不验证。
    pub id: String,
    /// 给人看的名字,如「小米13」。
    pub name: String,
}

/// 设备发给服务端的信令消息。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientSignal {
    /// 连上后的第一句:自报家门。不发这句就不会出现在任何人的名册里。
    Hello { device: DeviceDto },
    /// 转给另一台设备。`payload` 是 SDP 或 ICE 候选。
    Signal {
        /// 目标设备 id。**只发给它一台** —— 广播会让每台设备都以为自己被邀请。
        to: String,
        /// 对服务端**不透明**的一段文本。它是信令服务器,不是 WebRTC 的参与方,
        /// 解析这里等于把上游协议的演化绑到服务端上。
        payload: String,
    },
}

/// 服务端发给设备的信令消息。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerSignal {
    /// 当前在线的全部设备,含收信者自己 —— 谁该被过滤掉是显示问题,归客户端。
    ///
    /// 由服务端**主动推送**,每次名册变化都推。让客户端轮询的话,
    /// 一台设备下线到别人发现之间会有一段空窗,而那段时间里推流必然失败。
    Roster { devices: Vec<DeviceDto> },
    /// 另一台设备转来的信令,`payload` 原样。
    Signal { from: String, payload: String },
    /// 这条消息没能送到。
    ///
    /// 必须回,不能静默丢弃:主控发了 offer 就会等应答,丢了它会一直等下去。
    Error { code: String, message: String },
}
