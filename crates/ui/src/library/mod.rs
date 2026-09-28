//! 「我的库」的客户端侧:哪些歌在红心里,以及歌单的读与写。
//!
//! 与 [`crate::music`] 的分法照旧 —— 那边管的是「一批歌」,这边管的是「哪一批」。

// 屏蔽规则(#161):队列前进时跳过,菜单与设置页的建与删。
pub mod block;
pub mod feedback;
pub mod liked;
// 歌单列表与详情。与 artwork 同一道门:歌单封面要它,而它是原生 target 的依赖。
#[cfg(not(target_arch = "wasm32"))]
pub mod playlist;
// 标签选择器(#158)。不依赖 artwork,与 liked 一样各端共用一份代码。
pub mod tag;
