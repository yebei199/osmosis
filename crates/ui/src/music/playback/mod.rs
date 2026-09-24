//! 放哪一首,以及放的时候条上发生什么:控制条的绑定、一条播放意图的派发、
//! 起播与切歌的传输层、自动续播与下一首的预取。
//!
//! 与 `list` / `feed` 的分法:那两个管「摆出哪些歌」,这一组管「放哪一首」。
//!
//! 三份都从 `crate::music::*` 取邻居的条目,而不是 `use super::*` —— 拆分前
//! 它们本就在 music 那一层作用域里,这样写拆分不改任何可见性。

pub mod advance;
pub mod controls;
pub mod dispatch;
pub mod execution;
pub mod migrate;
pub mod transport;

// 互相看得见,music 那一层也看得见它们 —— 与拆分前同一个作用域。
pub(in crate::music) use self::{
    advance::*, controls::*, dispatch::*, execution::*,
    migrate::*, transport::*,
};
