//! 设备间的信令:WebSocket 的接入、设备名册、校时,以及组的全局播放状态(#142)。
//!
//! 模块名是同播留下的(同播已删,#137)。
//!
//! 与音乐那半毫无关系,state 也不共用 —— 两边唯一的交集是账号。

pub mod clock;
pub mod group;
pub mod roster;
pub mod signaling;
