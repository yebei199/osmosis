//! HTTP 路由的处理函数,按端点分组。路由表本身在 `main.rs`。

pub(crate) mod auth;
pub(crate) mod catalog_cache;
pub(crate) mod history;
pub(crate) mod likes;
pub(crate) mod lyric;
pub(crate) mod play;
pub(crate) mod playlists;
pub(crate) mod search;
