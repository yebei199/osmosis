//! 遥控的信令:WebSocket 的接入、设备名册、以及控制权归谁。
//!
//! 模块名是同播留下的(同播已删,#137)。
//!
//! 与音乐那半毫无关系,state 也不共用 —— 两边唯一的交集是账号。

pub mod control;
pub mod roster;
pub mod signaling;
