//! 整页的绑定:登录页、个人主页、搜索页。
//!
//! 「我的库」那几页归 [`crate::library`],音乐页归 [`crate::music`]。

pub mod account;
pub mod profile;
// 搜索的三个页签。歌曲那一路借 music 的队列,歌手与歌单各自成列。
#[cfg(not(target_arch = "wasm32"))]
pub mod search;
