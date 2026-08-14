//! 账号与收听历史的线上格式。

use serde::{Deserialize, Serialize};

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

/// `GET /stats` 的响应体:收听统计,服务端从播放事件流查询时聚合。
///
/// 没有「本月时长」:事件流只记起播、不记听了多久(见服务端 history 模块),
/// 编一个时长出来等于谎报。新增路由是兼容变更,不动 [`PROTOCOL_VERSION`]。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct StatsDto {
    /// 账号名,给个人主页当标题。
    pub username: String,
    /// 本月起播了多少次。
    pub month_plays: u32,
    /// 一共听过多少首不同的歌。
    pub distinct_tracks: u32,
    /// 连续在听的天数。今天还没听不清零,断更超过一天读作 0。
    pub streak_days: u32,
    /// 常听歌手,按播放次数从多到少,最多五个。
    pub top_artists: Vec<TopArtistDto>,
}

/// [`StatsDto`] 里的一个歌手条目。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct TopArtistDto {
    pub name: String,
    /// 出现在播放事件里的次数。
    pub plays: u32,
}
