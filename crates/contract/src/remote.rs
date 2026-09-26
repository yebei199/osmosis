//! 播放状态与输出路由这两个小枚举。
//!
//! 遥控器模式(命令集、被控端上报、迁移回话)已随 #142 退场:组里的设备对等,状态只在服务端
//! (`crate::GroupStateDto`)。留下的两样,一个是执行报告(`crate::QueueReportDto`)里的播放状态,
//! 一个是出声设备报给组里其他设备的输出路由(`crate::DeviceReportDto`)。

use serde::{Deserialize, Serialize};

/// 播放端此刻在干什么。
///
/// `Buffering` 与 `Playing` 必须分开:缓冲时进度并没有在走。
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
pub enum RemotePlayState {
    /// 什么都没放。
    Idle,
    /// 正在取直链、开流、解码 —— 还没出声。
    Buffering,
    Playing,
    Paused,
}

/// 输出路由。
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
pub enum OutputRouteDto {
    Speaker,
    Bluetooth,
    Wired,
}
