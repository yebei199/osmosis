//! 歌单的线上格式。两个来源合成一张列表(见 docs/adr/0016)。

use serde::{Deserialize, Serialize};

/// 一个歌单的来源。客户端靠它决定哪些操作可用 —— 本地歌单可改名可删,
/// 平台歌单只能取消收藏,「我喜欢的」两样都不行。
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistSource {
    /// 「我喜欢的」。它**就是**平台的红心列表,不是本地副本,
    /// 所以既不能删也不能改名(见 `CONTEXT.md`)。
    Liked,
    /// 音乐平台上的歌单:自建的或收藏来的。真相在平台那边。
    Platform,
    /// 本应用自己的歌单。平台不知道它存在。
    Local,
}

/// 一个歌单的元信息。曲目不在其中 —— 大歌单的曲目要另外分批取。
///
/// 两种来源的歌单归一成同一个形状(见 `docs/adr/0016`),差别只在 [`Self::source`]。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct PlaylistDto {
    pub source: PlaylistSource,
    /// 歌单标识。**只在同一个 `source` 内唯一** ——
    /// 本地歌单的 3 与平台歌单的 3 是两个东西。
    pub id: String,
    pub name: String,
    /// 封面图地址。本地歌单与「我喜欢的」目前没有封面,是 `None`。
    pub cover: Option<String>,
    /// 曲目总数,取自来源方而非已取到的条数。
    pub track_count: i32,
}

/// `GET /playlists` 的响应体:两个来源合并后的一张列表,「我喜欢的」在最前。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct PlaylistsDto {
    pub playlists: Vec<PlaylistDto>,
}
