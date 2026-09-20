//! 平台那边的东西:搜索、歌词、曲目缓存,以及网易云的账号绑定。
//!
//! 真相都在上游,这一侧只管取和缓存 —— 自家数据归 [`super::library`]。

pub(crate) mod catalog_cache;
pub(crate) mod lyric;
pub(crate) mod netease;
pub(crate) mod search;
