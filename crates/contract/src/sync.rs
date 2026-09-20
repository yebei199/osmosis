//! 同播的线上格式:设备名册与双向信令。

use serde::{Deserialize, Serialize};

use crate::{RemoteCommand, RemoteStateDto};

/// 一台在线设备。
///
/// 「在线」没有别的含义:它**等于**此刻与服务端之间存在活跃连接。
/// 服务端不记忆离线设备,所以名册里出现过就是现在能推流的(见 `docs/adr/0009`)。
///
/// **归属不在这里**:设备属于哪个账号由服务端从连接的 token 定,不由设备自报。
/// 让它自报的话,任何人都能把自己塞进别人的名册,而那不会报任何错。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct DeviceDto {
    /// 设备自己生成、本地保存的 id。在**同一个账号的桶内**唯一即可 ——
    /// 服务端不验证它,重名的后果也只波及自己那一桶。
    pub id: String,
    /// 给人看的名字,如「小米13」。
    pub name: String,
}

/// 设备发给服务端的信令消息。
///
/// 不派生 `Eq`:遥控的命令与上报里有音量那个 `f32`(见 [`RemoteCommand`])。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
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

    /// 接管这台设备:本机要当它的遥控器。
    ClaimControl {
        target: String,
        /// 重连之后**续**上手上那个代次,而不是重新接管。
        ///
        /// `None` 是用户主动按下的那一次:顶掉当前的遥控器,无论它是谁。
        /// `Some(g)` 是重连时自动重发的那一次:槽位仍是第 `g` 代才续,
        /// 否则回一条 [`ServerSignal::ControlRevoked`]。
        /// 不分这两种的话,「旧遥控器自动重连**不**夺回」那条产品规则
        /// 就落不了地 —— 断线的那台一回来就会把接管者顶掉。
        resume: Option<u64>,
    },
    /// 被控端按了「退出被遥控」:清掉自己身上的控制权。
    ///
    /// 只有**被控端**发得出这一条。遥控器想放手就选回本机,不必知会服务端 ——
    /// 它一走了之,被控端仍然该接着放(手机没电不能让 pc1 停)。
    ExitControlled,
    /// 遥控器发给被控端的一条命令。
    Command {
        /// 被控端的设备 id。
        to: String,
        /// 对服务端**不透明** —— 它只看 [`Self::Command::to`],不看这里一眼。
        cmd: RemoteCommand,
    },
    /// 被控端每秒一次的状态上报。发给谁由服务端从控制权槽位查,不由这里指定:
    /// 让被控端自己写目标的话,它能把状态推给任何一台设备。
    State { state: RemoteStateDto },
    /// 向被控端要一次完整状态。
    ///
    /// 取得控制权、切换目标、重连之后各要一次。服务端不缓存状态
    /// (`docs/adr/0030`),所以「现在是什么样」只能问被控端本人。
    SnapshotRequest { to: String },
}

/// 服务端发给设备的信令消息。
///
/// 不派生 `Eq`,理由同 [`ClientSignal`]。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
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

    /// 接管成功。`generation` 是这一次控制权的代次。
    ///
    /// 带着代次而不是只说一声"成了":旧遥控器重连时会重发 `ClaimControl`,
    /// 服务端按代次认得出谁才是当前那一个(见 `server::syncplay::control`)。
    ControlGranted { generation: u64 },
    /// 控制权没了。`by` 是把它拿走的那台设备 —— 另一台遥控器,或者
    /// 按了「退出被遥控」的被控端本人。
    ControlRevoked { by: String },
    /// 遥控器发来的一条命令,`cmd` 原样。
    Command { cmd: RemoteCommand },
    /// 被控端上报的状态,转给持权的遥控器。
    State { from: String, state: RemoteStateDto },
    /// 持权的遥控器要一次完整状态,立刻回一条 [`ClientSignal::State`]。
    ///
    /// 不带发信人:服务端只会把它从持权的那台转过来,被控端答复的去向
    /// 同样由槽位定,知道是谁问的没有用处。
    SnapshotRequest,
    /// 本机**没有**被谁遥控 —— 服务端槽位上查不到这条遥控关系。
    ///
    /// 被控端收到即解锁、撤横幅。非有不可:槽位可能在被控端不知情时就没了
    /// (服务端重启、遥控关系被别处撤掉),而被控端的锁定态只有它自己按
    /// 「退出被遥控」才清。结果是它挂着一条假横幅、每秒往一个没人收的地方
    /// 上报,本机的播放动作全被挡着,而横幅上那台设备早就不在遥控它了
    /// (#102 F-004)。
    ///
    /// 服务端在两处回它:收到无槽位的 [`ClientSignal::State`],以及收到
    /// 无槽位的 [`ClientSignal::ExitControlled`]。前者是自愈 —— 上报每秒一条,
    /// 所以最迟一秒就纠正过来;后者是让那一下「退出」不至于石沉大海。
    ///
    /// 新增一条服务端信令是兼容变更,[`crate::PROTOCOL_VERSION`] 不动:
    /// 老客户端不认得它,而它只在本来就是错的那个状态里发得出来。
    NotControlled,
    /// 本机被这台设备接管了:进入锁定态,界面上挂「正被 xx 遥控」。
    ///
    /// 带整个 `DeviceDto` 而不只是 id:横幅上要写的是人看得懂的名字,
    /// 而 id 是「主机名-进程号」。
    ControlledBy { device: DeviceDto },
}
