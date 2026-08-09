//! 契约:客户端与服务端之间在网络上传输的数据的**形状** ——
//! 请求体、响应体、错误码、协议版本号。
//!
//! 契约刻意不包含领域规则。"一个订单不能被取消两次"是领域规则,属于
//! `app-core` 或服务端;"取消请求携带一个订单 id 字段"才是契约。
//! 两侧各自维护自己的领域模型,只在这里相遇。见 `docs/adr/0001`。

use serde::{Deserialize, Serialize};

/// 协议版本。客户端与服务端就线上格式达成的约定的版本号。
///
/// 任何对本 crate 中类型的**不兼容**改动都必须让它加一:改字段名、删字段、
/// 改字段语义。新增可选字段是兼容的,不必加一。
///
/// 2:音乐相关的路由开始要求登录态(既有路由多了一个必需的请求头,老客户端会
/// 整片 401),`/search` 拆成 `/search/tracks`、`/search/artists`、
/// `/search/playlists` 三条。
pub const PROTOCOL_VERSION: u32 = 2;

/// `GET /health` 的响应体。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct HealthDto {
    /// 服务端自述的状态。目前恒为 `"ok"` —— 能返回就说明活着。
    pub status: String,
    /// 服务端所用的 [`PROTOCOL_VERSION`]。客户端据此判断双方是否说同一种话。
    pub protocol_version: u32,
}

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

/// `POST /register` 的请求体。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct RegisterDto {
    pub username: String,
    pub password: String,
    /// 部署时配置的邀请码。服务面向公网,没有它任何人都能开户。
    pub invite: String,
}

/// `POST /login` 的请求体。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct LoginDto {
    pub username: String,
    pub password: String,
}

/// `POST /register` 与 `POST /login` 的响应体:一个可用的会话。
///
/// token 由客户端本地长期保存,之后每次请求放进 `Authorization: Bearer`。
/// 服务端只存它的哈希,所以**这是它唯一一次出现**,丢了只能重新登录。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct SessionDto {
    pub token: String,
    /// 登录成功的账号名,回显给界面用,省客户端一次请求。
    pub username: String,
}

/// `POST /played` 的请求体:报告一次起播。
///
/// 只说"放了什么、什么时候"(时刻由服务端记),不说"听了多久" ——
/// 补记时长要靠客户端在退出或切歌时再发一次,而崩溃与断网时那一条就丢了。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct PlayedDto {
    pub platform: String,
    pub track_id: String,
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

/// 一台在线设备。
///
/// 「在线」没有别的含义:它**等于**此刻与服务端之间存在活跃连接。
/// 服务端不记忆离线设备,所以名册里出现过就是现在能推流的(见 `docs/adr/0009`)。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct DeviceDto {
    /// 设备自己生成、本地保存的 id。服务端信任自报,不验证。
    pub id: String,
    /// 给人看的名字,如「小米13」。
    pub name: String,
}

/// 设备发给服务端的信令消息。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientSignal {
    /// 连上后的第一句:自报家门。不发这句就不会出现在任何人的名册里。
    Hello { device: DeviceDto },
    /// 转给另一台设备。`payload` 是 SDP 或 ICE 候选。
    Signal {
        /// 目标设备 id。**只发给它一台** —— 广播会让每台设备都以为自己被邀请。
        to: String,
        /// 对服务端**不透明**的一段文本。它是信令服务器,不是 WebRTC 的参与方,
        /// 解析这里等于把上游协议的演化绑到服务端上。
        payload: String,
    },
}

/// 服务端发给设备的信令消息。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerSignal {
    /// 当前在线的全部设备,含收信者自己 —— 谁该被过滤掉是显示问题,归客户端。
    ///
    /// 由服务端**主动推送**,每次名册变化都推。让客户端轮询的话,
    /// 一台设备下线到别人发现之间会有一段空窗,而那段时间里推流必然失败。
    Roster { devices: Vec<DeviceDto> },
    /// 另一台设备转来的信令,`payload` 原样。
    Signal { from: String, payload: String },
    /// 这条消息没能送到。
    ///
    /// 必须回,不能静默丢弃:主控发了 offer 就会等应答,丢了它会一直等下去。
    Error { code: String, message: String },
}

/// 请求失败时的响应体。
///
/// HTTP 状态码只做粗分类(4xx 请求方的问题 / 5xx 服务端这边的问题),
/// 具体语义由 [`Self::code`] 承担 —— 客户端按 `code` 分支,不按状态码。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct ErrorDto {
    /// 稳定的机读错误码,如 `"netease_not_logged_in"`。
    ///
    /// 它是契约的一部分:改动一个已有的 code 等于改字段语义,要动
    /// [`PROTOCOL_VERSION`]。新增 code 是兼容的。
    pub code: String,
    /// 给人看的说明。客户端不应该拿它做分支判断。
    pub message: String,
}

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

#[cfg(test)]
mod tests {

    /// 旧版报文里没有新加的字段,不能因此整个解不出来。
    ///
    /// 服务端与客户端不是同时上线的:两边都能装着旧的那一半跑一阵。
    /// 少一个字段就整条响应失败的话,现象是「升级完 app 什么都拉不出来」,
    /// 而错误信息只会说"服务端的答复看不懂"。
    #[test]
    #[ignore = "骨架待评审"]
    fn a_tracks_response_without_the_new_field_still_parses()
     {
    }
}
