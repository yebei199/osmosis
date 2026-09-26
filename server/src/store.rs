//! 自家数据的家:连接池与迁移,以及账号、本地歌单、播放事件、平台曲目缓存。
//!
//! 这一层只认 Postgres —— HTTP 的形状归 [`crate::routes`],上游的东西归
//! [`crate::bangdream`]。缓存那一半存的是平台的东西,但它不是第二份真相,
//! 三条规矩见 `cache` 模块开头与 `docs/adr/0018`。

pub mod account;
pub mod archive;
pub mod cache;
pub mod db;
pub mod group;
pub mod history;
pub mod playlist;
pub mod queue;
