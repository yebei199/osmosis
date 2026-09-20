//! 「我的库」:红心、本地歌单、播放历史。
//!
//! 真相在自家 Postgres([`crate::store`]),曲目详情向 [`super::catalog`] 那侧借。

pub(crate) mod history;
pub(crate) mod likes;
pub(crate) mod playlists;
