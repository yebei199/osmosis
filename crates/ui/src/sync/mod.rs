//! 同播与遥控的界面状态:设备名册、推流/收听、以及遥控器那一侧。
//!
//! 两者共用同一条信令连接(见 `docs/adr/0030`)。
//! 整组只在原生 target 上编:wasm 没有 WebRTC 之外的音频栈可推。

pub mod remote;
pub mod syncplay;
