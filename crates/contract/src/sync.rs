//! 信令的线上格式:设备名册、握手与遥控的双向消息。

use serde::{Deserialize, Serialize};

use crate::{RemoteCommand, RemoteStateDto};

/// 一台在线设备。
///
/// 「在线」没有别的含义:它**等于**此刻与服务端之间存在活跃连接。
/// 服务端不记忆离线设备,所以名册里出现过就是现在能遥控的(见 `docs/adr/0009`)。
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

/// 一条信令消息的字节上限。
///
/// 服务端照这个数配 WebSocket(`server::syncplay::signaling`),所以它是**协议的
/// 一部分**,不是服务端的私事 —— 发送端不知道这个数就没法在发之前拦住自己。
///
/// 超限的后果比「这条消息没送到」重得多:tungstenite 读到超长消息返回
/// `Capacity(MessageTooLong)`,服务端读循环那句 `let Some(Ok(message)) = incoming
/// else { break }` 直接跳出,**整条连接就此断掉**,随后走出册与控制权清理 ——
/// 于是遥控器不但这一下没生效,还把自己的控制权弄丢了(#108)。
/// 所以发送端必须**发之前**量一次,不能发出去让它失败。
///
/// 限的是**完整的 WebSocket message**,不是单个 frame:原生分帧在末尾仍然合并
/// 计数,拆帧绕不过去。
///
/// 数值本身:遥控的命令与上报都是几百字节(当初按同播的 SDP 与 ICE 候选定的,
/// 那些也不过几 KiB),64 KiB 已经给得很松,而放大它意味着一条连接能让服务端
/// 为它单独攒出这么多内存。真正大的载荷
/// (整批曲目)该换一条路走,不该靠抬高这个数(见 #109)。
pub const MAX_SIGNAL_BYTES: usize = 64 * 1024;

/// 设备发给服务端的信令消息。
///
/// 不派生 `Eq`:遥控的命令与上报里有音量那个 `f32`(见 [`RemoteCommand`])。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientSignal {
    /// 连上后的第一句:自报家门,顺带说自己讲的是哪一版协议。
    ///
    /// 不发这句就不会出现在任何人的名册里。
    ///
    /// `protocol_version` 带 `#[serde(default)]`,**这一条是有意的**:旧客户端
    /// 发的 `Hello` 里没有它,而解不出来的话服务端只会静默丢掉整条消息,
    /// 那条连接就挂在「等 Hello」上直到十秒超时 —— 于是「版本不对」与
    /// 「网络不好」在两边都长得一模一样。默认成 0 才能让服务端**认出**
    /// 它是个旧端,并且明确地拒绝它(见 [`ServerSignal::Welcome`])。
    Hello {
        device: DeviceDto,
        #[serde(default)]
        protocol_version: u32,
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
    /// 只有**被控端**发得出这一条。遥控器选回本机是一次迁移(`BeginOutputs`
    /// 到 `CommitOutputs`,#137 ③),被控端在迁移里被叫停;遥控器一走了之(下线)
    /// 则什么都不发,被控端仍然该接着放(手机没电不能让 pc1 停)。
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
    ///
    /// 装箱的理由同 [`ServerSignal::State`]:上报带着当前曲目与迁移回话,比别的
    /// 变体大出几百字节,不装箱的话每条上行消息都按它占位。线上写法不变。
    State { state: Box<RemoteStateDto> },
    /// 向被控端要一次完整状态。
    ///
    /// 取得控制权、切换目标、重连之后各要一次。服务端不缓存状态
    /// (`docs/adr/0030`),所以「现在是什么样」只能问被控端本人。
    SnapshotRequest { to: String },
    /// 遥控器开始一次「改在这些设备播放」:`outputs` 是**换上之后**的输出集合。
    ///
    /// 这一步只登记,不换人:集合里新来的设备收到 [`ServerSignal::ControlledBy`]
    /// 开始听命令、开始上报,原来的成员照旧 —— 源停没停、目标起没起都还没
    /// 确认,这时候就把源从组里摘掉,就再也没有人能叫它停了(#137 ③)。
    ///
    /// 空集合是「改回本机」:本机输出不经服务端,组里只剩要被停掉的那些。
    /// 遥控器本机也可以在集合里(本机在放时加入别的设备,#137 ⑤),但集合只有它自己时
    /// 仍是单机输出，不经服务端。
    BeginOutputs {
        operation_id: String,
        outputs: Vec<String>,
        /// 换上之后谁当主端(持有组时间线、决定下一首的那一台),必须在 `outputs` 里。
        /// 不在或者缺省就取 `outputs` 的第一台。
        ///
        /// 主端被换下时这就是**显式的主端交接**(#137 ⑤):新主端从它手上最近那份共同
        /// 计划接着往下发，不是服务端替谁另选一个。
        #[serde(default)]
        master: Option<String>,
    },
    /// 那一次操作确认完了:输出集合正式换成 `BeginOutputs` 里那一份。
    ///
    /// 被换下来的成员收到 [`ServerSignal::NotControlled`] 解锁 —— 它们此前已经
    /// 各自确认停了声音,这一条只是撤锁,不是叫停。
    CommitOutputs {
        operation_id: String,
        /// 真正跟上的那几台(#137 ⑤):新来的里准备不了、开始失败的不进组，撤锁。
        /// 必须是 `BeginOutputs` 那一份的子集;缺省就是整份。
        #[serde(default)]
        outputs: Option<Vec<String>>,
    },
    /// 放弃那一次操作:新来的设备撤锁,组的成员集合不变。
    AbortOutputs { operation_id: String },
    /// 校时:服务端立刻回一条 [`ServerSignal::TimePong`],带上它此刻的单调时钟。
    ///
    /// 客户端自己记下发出与收到的本机时刻，取往返最短的那几次估偏移(#137 ⑤)。
    /// 服务端不参与估计，也不需要知道谁在校时。
    TimePing { id: u64 },
    /// 主端发布共同计划。服务端只认当前主端、当前任期发来的，转给组里其余成员与遥控器;
    /// 别的一律回错，不转。
    GroupPlan {
        term: u64,
        plan: Box<crate::GroupPlanDto>,
    },
}

/// 服务端发给设备的信令消息。
///
/// 不派生 `Eq`,理由同 [`ClientSignal`]。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerSignal {
    /// 握手应答:服务端说自己讲的是哪一版协议。
    ///
    /// **在入册之前发**,而入册是取得控制权的前提 —— 所以版本不对的那一端
    /// 在能够遥控任何设备之前就被挡住了(`docs/adr/0031`)。
    ///
    /// 一条变体覆盖四种组合:
    ///
    /// | 客户端 | 服务端 | 发生什么 |
    /// |---|---|---|
    /// | 新 | 新 | 收到 `Welcome{N}`,与本端的 [`crate::PROTOCOL_VERSION`] 对得上,照常入册 |
    /// | 旧 | 新 | 服务端认出 `protocol_version` 缺省的 0,发一条 `Welcome` 就关掉连接、**不入册**;旧端解不出这条消息,但它启动时的 `/health` 自检已经说过话了 |
    /// | 新 | 旧 | 这条**永远不来**。客户端在收到第一条 `Roster` 时还没见过它,据此判定对端太旧 —— 名册到得了,控制权申请由客户端自己挡下 |
    /// | 旧 | 旧 | 谁也不认识它,照旧 |
    ///
    /// 第三种是它非得在入册前发不可的原因:`Roster` 是入册后的第一条下行,
    /// 拿「先来的是谁」当判据才成立。
    Welcome { protocol_version: u32 },
    /// 当前在线的全部设备,含收信者自己 —— 谁该被过滤掉是显示问题,归客户端。
    ///
    /// 由服务端**主动推送**,每次名册变化都推。让客户端轮询的话,
    /// 一台设备下线到别人发现之间会有一段空窗,而那段时间里接管必然失败。
    Roster { devices: Vec<DeviceDto> },
    /// 这条消息没能送到。
    ///
    /// 必须回,不能静默丢弃:遥控器发了接管就会等答复,丢了它会一直等下去。
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
    ///
    /// 装箱不是为了省内存,是为了别让**别的变体**跟着它一起变大:枚举按最大
    /// 的那个变体占位,而 `Roster`、`SnapshotRequest` 这些小家伙与它共用一条
    /// 通道(`OUTBOX_CAPACITY = 32`)。上报里那份曲目展示摘要让这个变体比其余
    /// 的大出两百多字节,clippy 的 `large_enum_variant` 正是为此。
    ///
    /// 线上写法一个字节没变:`Box` 在 serde 那里是透明的。
    State {
        from: String,
        state: Box<RemoteStateDto>,
    },
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
    /// `BeginOutputs` 登记上了。`generation` 是这一位遥控器的控制代次 ——
    /// 与 [`Self::ControlGranted`] 同一个数,重连续权用它。
    OutputsBegun {
        operation_id: String,
        generation: u64,
    },
    /// `CommitOutputs` 生效了。`term` 是换人之后的主端任期:成员集合每换
    /// 一次加一,旧任期里迟到的一切都不再作数。
    OutputsCommitted { operation_id: String, term: u64 },
    /// 校时的回话。`server_us` 是服务端单调时钟(微秒);`epoch` 是这个钟的纪元,
    /// 服务端每次启动换一个 —— 换了就说明旧的偏移估计与旧计划里的时刻全都作废。
    TimePong {
        id: u64,
        server_us: u64,
        epoch: u64,
    },
    /// 组现在的样子:任期、主端、成员(含进行中那一次拉进来的)。
    ///
    /// 发给组里每一台与遥控器。成员凭它知道该听谁的计划;主端看见新成员就把手上的计划
    /// 再发一遍，让新来的跟上;被指定成主端的那一台从这一刻起负责往下发(#137 ⑤)。
    Group {
        term: u64,
        master: Option<String>,
        members: Vec<String>,
    },
    /// 当前主端发布的共同计划，原样转来。
    GroupPlan {
        from: String,
        term: u64,
        plan: Box<crate::GroupPlanDto>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同播的 SDP/ICE 转发已经从协议里拿掉(#137):两个方向的 `signal`
    /// 都解不出来。解得出来的话,服务端仍会替一个早已不存在的功能转发
    /// 不透明载荷,那是一条谁都能用、谁都不再检查的通道。
    #[test]
    fn a_webrtc_relay_message_is_no_longer_part_of_the_protocol()
     {
        let upstream = serde_json::from_str::<ClientSignal>(
            r#"{"type":"signal","to":"b","payload":"v=0"}"#,
        );
        let downstream = serde_json::from_str::<ServerSignal>(
            r#"{"type":"signal","from":"a","payload":"v=0"}"#,
        );

        assert!(
            upstream.is_err(),
            "上行 signal 还解得出来: {upstream:?}"
        );
        assert!(
            downstream.is_err(),
            "下行 signal 还解得出来: {downstream:?}"
        );
    }

    /// 换输出那三条上行与两条下行都解得回来。
    #[test]
    fn output_membership_signals_round_trip() {
        let upstream = [
            ClientSignal::BeginOutputs {
                operation_id: "op-1".to_owned(),
                outputs: vec!["pc1".to_owned()],
                master: Some("pc1".to_owned()),
            },
            ClientSignal::BeginOutputs {
                operation_id: "op-2".to_owned(),
                outputs: Vec::new(),
                master: None,
            },
            ClientSignal::CommitOutputs {
                operation_id: "op-1".to_owned(),
                outputs: Some(vec!["pc1".to_owned()]),
            },
            ClientSignal::AbortOutputs {
                operation_id: "op-1".to_owned(),
            },
        ];
        for message in upstream {
            let back: ClientSignal = serde_json::from_str(
                &serde_json::to_string(&message)
                    .expect("消息该能序列化"),
            )
            .expect("消息该能解回来");
            assert_eq!(back, message);
        }

        let downstream = [
            ServerSignal::OutputsBegun {
                operation_id: "op-1".to_owned(),
                generation: 4,
            },
            ServerSignal::OutputsCommitted {
                operation_id: "op-1".to_owned(),
                term: 2,
            },
        ];
        for message in downstream {
            let back: ServerSignal = serde_json::from_str(
                &serde_json::to_string(&message)
                    .expect("消息该能序列化"),
            )
            .expect("消息该能解回来");
            assert_eq!(back, message);
        }
    }
}
