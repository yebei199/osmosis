//! 信令的线上格式:设备名册、握手、校时,以及组状态的广播(#142)。

use serde::{Deserialize, Serialize};

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

/// 设备发给服务端的信令消息。点歌、切歌这些意图不走这里,走 HTTP(`/group/*`,#142)。
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
    /// 校时:服务端立刻回一条 [`ServerSignal::TimePong`],带上它此刻的单调时钟。
    ///
    /// 客户端自己记下发出与收到的本机时刻，取往返最短的那几次估偏移(#137 ⑤)。
    /// 服务端不参与估计，也不需要知道谁在校时。
    TimePing { id: u64 },
    /// 出声设备每秒一条的执行事实(#142)。服务端转给组里其他在线成员。
    Report { report: crate::DeviceReportDto },
}

/// 服务端发给设备的信令消息。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerSignal {
    /// 握手应答:服务端说自己讲的是哪一版协议。
    ///
    /// **在入册之前发**:版本不对的那一端进不了名册,也就收不到组状态、发不了意图
    /// (`docs/adr/0031`)。
    ///
    /// 一条变体覆盖四种组合:
    ///
    /// | 客户端 | 服务端 | 发生什么 |
    /// |---|---|---|
    /// | 新 | 新 | 收到 `Welcome{N}`,与本端的 [`crate::PROTOCOL_VERSION`] 对得上,照常入册 |
    /// | 旧 | 新 | 服务端认出 `protocol_version` 缺省的 0,发一条 `Welcome` 就关掉连接、**不入册**;旧端解不出这条消息,但它启动时的 `/health` 自检已经说过话了 |
    /// | 新 | 旧 | 这条**永远不来**。客户端在收到第一条 `Roster` 时还没见过它,据此判定对端太旧 —— 名册到得了,客户端自己报「版本对不上」 |
    /// | 旧 | 旧 | 谁也不认识它,照旧 |
    ///
    /// 第三种是它非得在入册前发不可的原因:`Roster` 是入册后的第一条下行,
    /// 拿「先来的是谁」当判据才成立。
    Welcome { protocol_version: u32 },
    /// 当前在线的全部设备,含收信者自己 —— 谁该被过滤掉是显示问题,归客户端。
    ///
    /// 由服务端**主动推送**,每次名册变化都推。
    Roster { devices: Vec<DeviceDto> },
    /// 这条消息没能处理。
    Error { code: String, message: String },

    /// 校时的回话。`server_us` 是服务端单调时钟(微秒);`epoch` 是这个钟的纪元,
    /// 服务端每次启动换一个 —— 换了就说明旧的偏移估计与旧计划里的时刻全都作废。
    TimePong { id: u64, server_us: u64, epoch: u64 },
    /// 组的全局播放状态(#142)。入册之后先推一份,之后版本一变就推给账号下每台在线设备。
    /// 组散了(或者从来没有)是 `None`。装箱:它比别的变体大出一截。
    GroupState {
        state: Option<Box<crate::GroupStateDto>>,
    },
    /// 组里某台出声设备的执行事实,原样转来。
    DeviceReport {
        from: String,
        report: crate::DeviceReportDto,
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
}
