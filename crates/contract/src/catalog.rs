//! 目录侧的线上格式:曲目、歌手、搜索结果、播放源与歌词。

use serde::{Deserialize, Serialize};

use crate::PlaylistDto;

/// 一首可播放的歌曲。
///
/// 只保留客户端此刻用得上的字段。上游(bang-dream)的 `Track` 还有音质规格、
/// 付费等级等等 —— 用到时再加,新增可选字段是兼容变更,不必动 [`PROTOCOL_VERSION`]。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct TrackDto {
    /// 歌曲所属的平台,如 `"netease"`。
    ///
    /// 只有一个平台时它恒为同一个值,看着多余 —— 但歌曲的身份**本来就是**
    /// `(平台, 平台内 id)`,少了它,接第二个平台时两边的 id 会静默撞车。
    pub platform: String,
    /// 平台内的歌曲 id。与 [`Self::platform`] 合起来才唯一。
    pub id: String,
    pub title: String,
    /// 别名或副标题,常见于日文原名。平台没给就是 `None`。
    pub alias: Option<String>,
    /// 歌手名。保持列表形态 —— 怎么拼接是显示问题,属于 UI。
    pub artists: Vec<String>,
    /// 封面图地址。平台没给就是 `None`,不用空串冒充。
    pub cover: Option<String>,
    /// 时长,毫秒。与上游同单位,不做换算。
    pub duration_ms: i64,
}

/// `GET /search/tracks` 的响应体。
///
/// 三类搜索各有一条路由、各有一个响应类型,而不是一条路由带 `?type=`:
/// URL 与响应形状因此是**同一个决定**,不是两个必须彼此对上的决定。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct SearchDto {
    pub tracks: Vec<TrackDto>,
    /// 还有没有下一页。翻页由客户端持有 offset 自行推进。
    pub has_more: bool,
}

/// 一个歌手。
///
/// 内嵌在 [`TrackDto`] 里的歌手只是个名字(那里 `artists` 是 `Vec<String>`) ——
/// 那是显示用的;这里是搜索结果里的歌手**实体**,能点进去。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct ArtistDto {
    pub id: String,
    pub name: String,
    /// 头像。平台没给就是 `None`,不用空串冒充。
    pub avatar: Option<String>,
    /// 专辑数,取自平台。
    pub album_count: i32,
}

/// `GET /search/artists` 的响应体。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct ArtistSearchDto {
    pub artists: Vec<ArtistDto>,
    pub has_more: bool,
}

/// `GET /search/playlists` 的响应体。
///
/// 不复用 [`PlaylistsDto`]:那是「我的歌单」那张合并列表,没有翻页;
/// 搜索结果有。硬塞一个恒为 false 的字段等于让客户端解读一个没有意义的信号。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct PlaylistSearchDto {
    pub playlists: Vec<PlaylistDto>,
    pub has_more: bool,
}

/// 一批曲目。`GET /daily` 与 `GET /liked` 的响应体。
///
/// 不复用 [`SearchDto`]:它带 `has_more`,而每日推荐本来就只有固定一批,
/// 硬塞一个恒为 false 的字段等于让客户端去解读一个没有意义的信号。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct TracksDto {
    pub tracks: Vec<TrackDto>,
    /// 平台给不出详情、因此没能出现在 `tracks` 里的曲目有几首。
    ///
    /// 它们的成员关系写不进缓存(外键要求先有详情),所以只能不给 —— 但要报出
    /// 数目,否则歌单静默变短,用户分不清「我少点了一个红心」和「平台不给这首歌
    /// 的详情」。常态是 0。
    ///
    /// `serde(default)` 是必需的:服务端与客户端不同时上线,旧的那一半发出来的
    /// 报文没有这个字段,不能因此整条解不出来。
    #[serde(default)]
    pub unavailable: usize,
}

/// `GET /play/{track_id}` 的响应体:一次取到的可播放源。
///
/// 这些字段属于"这次取到的源"而非歌曲本身 —— 换个音质档位再取会得到不同的值。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct PlaySourceDto {
    /// 平台签发的临时直链。带签名、会过期,**不要缓存**。
    pub url: String,
    /// 音频容器格式,如 `"mp3"`、`"flac"`。
    pub format: String,
    /// 码率,bit/s。
    pub bit_rate: i32,
    /// 是否只是试听片段(通常 30 秒)。
    ///
    /// 必须送到客户端:不告诉用户的话,一首歌放到 30 秒就停会被当成播放器坏了。
    pub trial: bool,
}

/// 歌词的一行。
///
/// 时间轴取**行级**:上游同时提供逐字(`LyricWord`),但那一档要等行级链路
/// 跑通后另议(见 issue #16)。届时加一个可空的 `words` 字段即可,是兼容变更。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct LyricLineDto {
    /// 本行开始的时刻,毫秒,相对歌曲开头。与上游同单位,不做换算。
    pub start_ms: i64,
    /// 本行结束的时刻,毫秒。上游没给时与 `start_ms` 相同 ——
    /// 「当前是第几行」由下一行的开始时刻决定,不依赖这个字段。
    pub end_ms: i64,
    pub text: String,
    /// 译文。没有翻译的歌就是 `None`,不用空串冒充。
    pub translation: Option<String>,
}

/// `GET /lyric/{track_id}` 的响应体。
///
/// **空行表不是失败**:纯音乐、上游未收录都会给出空表,客户端据此隐藏歌词区,
/// 而不是弹一个错误 —— 「这首歌没有歌词」是正常状态,不是故障。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct LyricDto {
    pub lines: Vec<LyricLineDto>,
}

/// `GET /liked/ids` 的响应体:红心的**全量标识**。
///
/// 与 [`TracksDto`] 是两件事:那个是一页曲目,这个是一个集合。界面每一行都要问
/// 「这一首红心没有」,而分页的曲目回答不了这个问题。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct TrackIdsDto {
    /// 平台内的曲目 id。目前只有网易云一个平台,故不带平台名 ——
    /// 接第二个平台时这里要变成 `(平台, id)` 对,那是不兼容变更。
    pub track_ids: Vec<String>,
}
