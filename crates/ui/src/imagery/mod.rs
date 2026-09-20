//! 封面字节变成可显示的图,以及记住它。
//!
//! 三份分开是因为键不同:`cover` 只管解码,`artwork` 按歌单 id 存,
//! `thumbnail` 按封面 URL 存 —— 键不同,缓存、去重与淘汰的规则就全都不同。
//! 取字节那一步不在这里,归 `api`。
//!
//! 整组只在原生 target 上编:解码要 image,而 web 的封面等播放链路通了一起做。

pub mod artwork;
pub mod cover;
pub mod thumbnail;
