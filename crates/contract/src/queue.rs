//! 服务端持久播放队列的线上格式(见 `docs/adr/0031`)。
//!
//! 与 [`crate::remote`] 的分工:那边是遥控器与被控端之间的小消息,走 WebSocket;
//! 这边是队列本身,走 HTTP —— 曲目数据的体积随用户的歌单长度增长,而信令通道
//! 是按几百字节的小消息设计的(见 [`crate::MAX_SIGNAL_BYTES`])。

/// 一个队列版本最多几条。
///
/// 超出要**明确拒绝**,不截断、不静默:截断悄悄改了用户点的那一批是什么
/// (`docs/adr/0031` 三)。两侧共用这一个数 —— 客户端据它在上传之前就说得出
/// 「这一批太长」,服务端据它拒绝,两边各写一个字面量的话迟早对不上。
///
/// 五千首:网易云单个歌单的上限是一万,但一次点播把一万首冻结成一个版本
/// 不是本轮要支持的场景,真撞到了再谈分页上传。
pub const MAX_QUEUE_ENTRIES: usize = 5_000;

/// 一次分页读取最多几条。
///
/// 一页 500 条约 110 KiB —— 不走信令通道所以不撞 64 KiB,但也没必要一次
/// 把五千首塞进一个响应体里。
pub const QUEUE_PAGE_LIMIT: usize = 500;

use serde::{Deserialize, Serialize};

use crate::{RemotePlayState, TrackDto};

/// 新建一个队列。
///
/// 上传的就是 [`TrackDto`],不另设一个「队列条目输入」类型:点播那一刻客户端
/// 手上拿着的正是这一批 `TrackDto`,为它再翻译一次只会多一处能写错的地方。
/// 服务端把其中的展示字段抄进条目快照(`docs/adr/0031` 五)。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct CreateQueueDto {
    /// 这个队列归哪台设备的播放会话。不是 `account_id` —— 同账号两台设备
    /// 各自本机播放不该互相覆盖。
    pub device_id: String,
    pub tracks: Vec<TrackDto>,
}

/// 改一个队列:整份新内容,外加它基于哪一版。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct PublishQueueDto {
    /// 基于哪一版改的。对不上整次拒绝,回 `revision_conflict` ——
    /// 后到的那次不能凭「我也有一份完整列表」把别人的新版本盖掉。
    pub expected_revision: i64,
    pub tracks: Vec<TrackDto>,
}

/// 一次发布的产物。
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
)]
pub struct QueueRefDto {
    pub queue_id: i64,
    pub revision: i64,
}

/// 队列里的一条。
///
/// `entry_id` 而不是下标:队列允许同一首歌出现多次,下标会随插入删除整体
/// 挪位,而播放端手上那个「正在放第几条」必须跨版本认得出来。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct QueueEntryDto {
    pub entry_id: i64,
    /// 展示原序里的位置。**不是播放次序** —— 那是执行状态,在
    /// [`QueueReportDto::play_order`] 里。
    pub position: i64,
    pub track: TrackDto,
}

/// 按固定版本读回来的一页。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct QueuePageDto {
    pub queue_id: i64,
    /// 这一页属于哪一版。请求哪一版就是哪一版 —— 不会前半页旧顺序、
    /// 后半页新顺序。
    pub revision: i64,
    /// 这一版一共几条。客户端据它知道还要不要翻下一页。
    pub total: i64,
    pub offset: i64,
    pub entries: Vec<QueueEntryDto>,
}

/// 待应用的播放意图此刻处在哪一档。
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
pub enum QueueIntentState {
    /// 已提交,播放端还没确认应用。界面据此标「新版本待应用」——
    /// 数据库里写了不等于音箱在响。
    Pending,
    Applied,
    /// 播放端试过了,没成。旧执行副本仍然留着,不谎报已应用。
    Failed,
}

/// 第二层:待应用的播放意图。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct QueueIntentDto {
    pub device_id: String,
    pub revision: i64,
    pub entry_id: i64,
    /// 由发起方生成。重试同一次操作不该再次重置播放,服务端只比对它。
    pub operation_id: String,
    pub state: QueueIntentState,
    pub reason: Option<String>,
}

/// 下一条要应用的播放意图。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct SetQueueIntentDto {
    pub device_id: String,
    pub revision: i64,
    pub entry_id: i64,
    pub operation_id: String,
}

/// 一次操作的下场,搭着执行报告一起回。
///
/// 不单开一条路由:播放端知道「我到哪了」与「那次操作成没成」是同一刻的事,
/// 分两次发就会出现两者互相矛盾的中间态。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct QueueOperationOutcomeDto {
    pub operation_id: String,
    pub applied: bool,
    pub reason: Option<String>,
}

/// 第三层:播放端最近确认的执行状态。**是一份报告,不是实时真相。**
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct QueueReportDto {
    pub device_id: String,
    /// 播放端进程启动时的毫秒挂钟,重启换一个。与 [`Self::state_seq`] 组成
    /// 一个可比的序,旧 epoch 或倒退的序号一律拒 —— 少了它,上一条连接上
    /// 飘来的残余报告会把新进程的状态盖掉。
    pub epoch: i64,
    pub state_seq: i64,
    /// 播放端**实际应用**的版本。与队列的最新版本分开:下载失败时它停在
    /// 旧值上,而那正是要能看出来的事。
    pub applied_revision: i64,
    pub entry_id: Option<i64>,
    /// 实际播放次序:`entry_id` 的排列。显式报,不让两端凭 seed 猜出同一个
    /// 排列(`docs/adr/0031` 六)。
    ///
    /// **只在洗牌或回卷改变它时才带**,`None` 是「沿用服务端手上那份」。
    /// 每秒那条上报走 `None` —— 带上的话每秒要重写五千个 bigint,而线上
    /// 字节数并不会涨,于是 AC-2 照过而写放大全落在库里。
    pub play_order: Option<Vec<i64>>,
    /// 列表循环的轮次。随机每轮重洗,排列与轮次要一起看。
    pub round: i64,
    pub position_ms: i64,
    pub state: RemotePlayState,
    pub operation: Option<QueueOperationOutcomeDto>,
}

/// 一份报告收没收下。
///
/// 回一个布尔而不是 409:被拒的报告不是错误,是**正常的乱序**,而播放端对它
/// 唯一该做的事是继续报下一条。回错误的话客户端要么重试(更乱)、要么打一条
/// 看起来像故障的日志。
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
)]
pub struct QueueReportAckDto {
    pub accepted: bool,
}

/// 队列此刻的概况:最新版本、条目数,以及另外两层各自的最新一条。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct QueueHeadDto {
    pub queue_id: i64,
    pub revision: i64,
    pub total: i64,
    pub intent: Option<QueueIntentDto>,
    pub report: Option<QueueReportDto>,
}
