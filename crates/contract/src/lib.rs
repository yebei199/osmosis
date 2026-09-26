//! 契约:客户端与服务端之间在网络上传输的数据的**形状** ——
//! 请求体、响应体、错误码、协议版本号。
//!
//! 契约刻意不包含领域规则。"一个订单不能被取消两次"是领域规则,属于
//! `app-core` 或服务端;"取消请求携带一个订单 id 字段"才是契约。
//! 两侧各自维护自己的领域模型,只在这里相遇。见 `docs/adr/0001`。

use serde::{Deserialize, Serialize};

mod account;
mod catalog;
mod download;
mod group;
mod netease;
mod playlist;
mod queue;
mod remote;
mod sync;

pub use account::*;
pub use catalog::*;
pub use download::*;
pub use group::*;
pub use netease::*;
pub use playlist::*;
pub use queue::*;
pub use remote::*;
pub use sync::*;

/// 协议版本。客户端与服务端就线上格式达成的约定的版本号。
///
/// 任何对本 crate 中类型的**不兼容**改动都必须让它加一:改字段名、删字段、
/// 改字段语义。新增可选字段是兼容的,不必加一。
///
/// 2:音乐相关的路由开始要求登录态(既有路由多了一个必需的请求头,老客户端会
/// 整片 401),`/search` 拆成 `/search/tracks`、`/search/artists`、
/// `/search/playlists` 三条。
///
/// 3:播放队列挪进服务端(`docs/adr/0031`)。`RemoteCommand::Play` 不再拖着
/// 整批曲目,改带 `queue_id`/`revision`/`entry_id`/`operation_id`;
/// `RemoteStateDto` 删掉 `queue`、`queue_index` 与 `sent_at`,换成小状态
/// (队列标识、两个 revision、`entry_id`、长度,以及 `epoch` + `state_seq`
/// 这一对顺序键)。两样都是删字段改语义,不兼容。
///
/// 4:同播(WebRTC 推流)删除(#137,`docs/adr/0008` 废止)。`ClientSignal::Signal`
/// 与 `ServerSignal::Signal` 两个变体没了:协议 3 的客户端还会发 SDP 转发,
/// 新服务端不再认。删变体,不兼容。
///
/// 5:选设备改成迁移当前播放(#137 ③)。遥控器多了 `Prepare`/`Start`/`Stop`/
/// `Cancel` 四条命令与 `BeginOutputs`/`CommitOutputs`/`AbortOutputs` 三条组操作,
/// 上报多了迁移回话 `operation`,发布队列的响应多了 `entry_ids`。协议 4 的
/// 遥控器只会发 `ClaimControl`,选完设备什么都不迁、被控端也等不到停止 ——
/// 正是 ③ 要修掉的那一套,所以不兼容、成套升级。
///
/// 6:多成员同步播放(#137 ⑤)。多了校时(`TimePing`/`TimePong`)、主端发布的共同计划
/// (`GroupPlan`)与组通告(`Group`);`BeginOutputs` 多了 `master`(显式主端交接)。
/// 协议 5 的成员不会校时、不认共同计划，进了组只会各放各的，而一起出声正是这一版
/// 要保证的东西，所以不兼容、成套升级。
///
/// 7:服务端持有组的全局播放状态,组里设备对等可控(#142)。多了 `GroupState` /
/// `DeviceReport` 两条下行、`Report` 一条上行与 `/group/*` 意图路由;遥控接管、被控锁、
/// 主端计划那一套退场。协议 6 的客户端还在等主端发计划,进了组谁也不出声,所以不兼容。
///
/// **光改这个常量不够。** 版本比对此前只发生在 `/health`,而 `/signal` 的
/// 握手不看版本 —— 旧客户端照样连得上,两边遇到不认识的 JSON 默默丢弃,
/// 症状是「按了没反应」。拒绝要落在**取得控制权之前**,见
/// [`ClientSignal::Hello`] 的 `protocol_version` 与
/// [`ServerSignal::Incompatible`]。
pub const PROTOCOL_VERSION: u32 = 7;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧版报文里没有新加的字段,不能因此整个解不出来。
    ///
    /// 服务端与客户端不是同时上线的:两边都能装着旧的那一半跑一阵。
    /// 少一个字段就整条响应失败的话,现象是「升级完 app 什么都拉不出来」,
    /// 而错误信息只会说"服务端的答复看不懂"。
    #[test]
    fn a_tracks_response_without_the_new_field_still_parses()
     {
        // 旧服务端发出来的那份:只有 tracks
        let dto: TracksDto =
            serde_json::from_str(r#"{"tracks":[]}"#)
                .expect("少一个字段不该让整条响应解不出来");

        assert_eq!(
            dto.unavailable, 0,
            "没提这件事就是一首都没少"
        );
    }
}
