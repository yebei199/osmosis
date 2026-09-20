//! 「我的库」的客户端侧:哪些歌在红心里,以及歌单的读与写。
//!
//! 与 [`crate::music`] 的分法照旧 —— 那边管的是「一批歌」,这边管的是「哪一批」。

pub mod liked;
// 歌单列表与详情。与 artwork 同一道门:歌单封面要它,而它是原生 target 的依赖。
#[cfg(not(target_arch = "wasm32"))]
pub mod playlist;
