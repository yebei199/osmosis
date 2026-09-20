//! HTTP 路由的处理函数,按端点分组。路由表本身在 `main.rs`。

pub(crate) mod auth;
pub(crate) mod catalog;
pub(crate) mod library;
pub(crate) mod play;

#[cfg(test)]
pub(crate) mod testing;
