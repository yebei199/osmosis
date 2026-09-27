//! 帧循环这一侧:每帧驱动各渲染器、决定要不要下一帧、给一帧的耗时记账、把歌词推给界面前先去重。
//!
//! 这里不做任何业务判断 —— 推什么由各绑定模块定,这一层只管「这一帧做多少事」。

pub mod frame_stats;
pub mod lyric_push;
pub(crate) mod pace;
pub mod render_loop;
pub(crate) mod trace;
