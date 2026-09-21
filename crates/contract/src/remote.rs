//! 遥控器模式的线上格式:命令集,与被控端的状态上报。
//!
//! 遥控器模式与同播(`docs/adr/0008`)相反:同播是主控出声、听众收流、控制
//! 单向归主控;这里是**遥控器不出声**,被控端自己拉直链自己播,控制权归
//! 遥控器。两者共用同一条信令连接与同一份名册,是同一账号上的两种会话形态
//! (见 `docs/adr/0030`)。
//!
//! 服务端仍然不解释这里的任何一个字节:它只认 [`crate::ClientSignal::Command`]
//! 外层的 `to`,把 `cmd` 原样转走。类型定在契约里是为了让**两个客户端**对上,
//! 不是为了让服务端读它。

use serde::{Deserialize, Serialize};

use crate::TrackDto;

/// 遥控器发给被控端的一条命令。
///
/// 第一期只有这七条 —— 控制条上有的那些。随机与循环没进来:它们是队列的
/// 属性,而队列归被控端,遥控器此刻只显示、不改。
///
/// 不派生 `Eq`:[`Self::Volume`] 带一个 `f32`。音量本来就是 0..1 的连续量,
/// 为了一个 derive 把它折成整数百分比,等于在契约里留一次两端都要做的换算。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemoteCommand {
    /// 播这份列表,从第 `index` 首开始。
    ///
    /// 整批发过去而不是只发一首 id:自动续播在被控端发生(见 `docs/adr/0030`),
    /// 它得自己拿着后面那些歌 —— 遥控器锁屏、断线都不该让 pc1 停在一首上。
    Play {
        tracks: Vec<TrackDto>,
        index: usize,
    },
    Pause,
    Resume,
    Next,
    Prev,
    /// 跳到第 `ms` 毫秒。裁决由被控端回(`docs/adr/0019`)——
    /// 跳不跳得动只有拿着那条流的那端知道。
    Seek {
        ms: u64,
    },
    /// 音量,0..=1。
    Volume {
        level: f32,
    },
}

impl RemoteCommand {
    /// 一行日志里怎么称呼这条命令。
    ///
    /// 定在契约里而不是各端各写一份:遥控链路跨三个进程四跳(遥控器提交、
    /// 客户端入队、服务端转发、被控端收到),四条日志说的必须是同一件事,
    /// 否则拿 `grep` 把一次点歌串起来时对不上。
    ///
    /// `Play` 报**批次长度**而不是曲名:一条命令拖着整批歌,而那个数正是
    /// 它与别的命令唯一的区别 —— 别的变体大小固定,只有它随用户手上那个
    /// 列表增长(见 `Self::Play` 的说明)。
    pub fn summary(&self) -> String {
        match self {
            Self::Play { tracks, index } => format!(
                "play(批次 {} 首, 第 {index} 首)",
                tracks.len()
            ),
            Self::Pause => "pause".to_owned(),
            Self::Resume => "resume".to_owned(),
            Self::Next => "next".to_owned(),
            Self::Prev => "prev".to_owned(),
            Self::Seek { ms } => format!("seek({ms}ms)"),
            Self::Volume { level } => {
                format!("volume({level:.2})")
            }
        }
    }
}

/// 被控端此刻在干什么。
///
/// `Buffering` 与 `Playing` 必须分开:遥控器在两次上报之间按本地时钟插值,
/// 而缓冲时进度并没有在走 —— 不分的话手机上的进度条会自己往前爬,
/// 然后在下一次上报时跳回去。
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
pub enum RemotePlayState {
    /// 什么都没放。
    Idle,
    /// 正在取直链、开流、解码 —— 还没出声。
    Buffering,
    Playing,
    Paused,
}

/// 被控端每秒上报一次的全部状态。
///
/// **进度靠上报,不靠遥控器推算**(`docs/adr/0030`):各设备网速不同,
/// 「点了播」到「真出声」之间的延迟不可知,遥控器推算出来的位置必然与
/// 音箱里的声音对不上,而且不会有任何报错。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct RemoteStateDto {
    /// 正在放的那首。什么都没放时是 `None`,不用一首空歌冒充 ——
    /// 那和「这首的标题没取到」在界面上看起来一模一样。
    pub track: Option<TrackDto>,
    /// 播放位置,毫秒。
    pub position_ms: u64,
    pub state: RemotePlayState,
    /// 被控端手上的整个队列。遥控器只显示它,不持有自己的那一份。
    pub queue: Vec<TrackDto>,
    /// 在 [`Self::queue`] 里的位置。
    pub queue_index: usize,
    /// 被控端的音量,0..=1 —— 遥控器上的滑块读的是这个数。
    pub volume: f32,
    /// 被控端发出这条时它自己的挂钟毫秒。
    ///
    /// **不用来对时**:两台设备的钟本来就不一样,拿它算延迟只会算出负数。
    /// 它只回答「这条比那条新吗」—— 重连之后旧连接上的残余上报可能后到,
    /// 拿它盖掉新的,进度条就会倒退一次。过期与否由收信方按自己的钟判
    /// (见 `app_core::Output`)。
    pub sent_at: u64,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{ClientSignal, DeviceDto, ServerSignal};

    fn track(id: &str) -> TrackDto {
        TrackDto {
            platform: "netease".to_owned(),
            id: id.to_owned(),
            title: format!("歌 {id}"),
            alias: None,
            artists: vec!["LiSA".to_owned()],
            cover: None,
            duration_ms: 234_000,
        }
    }

    /// 命令的线上写法就是契约本身:两端各自按 `type` 分支。
    ///
    /// 改一个标签名等于换一条协议,而症状是「按了没反应」—— 对端解不出来时
    /// 静默丢弃(见 `server::syncplay::signaling::dispatch`),不会有任何报错。
    #[test]
    fn every_command_round_trips_through_its_tag() {
        let commands = [
            RemoteCommand::Play {
                tracks: vec![track("1"), track("2")],
                index: 1,
            },
            RemoteCommand::Pause,
            RemoteCommand::Resume,
            RemoteCommand::Next,
            RemoteCommand::Prev,
            RemoteCommand::Seek { ms: 42_000 },
            RemoteCommand::Volume { level: 0.35 },
        ];

        for command in commands {
            let text = serde_json::to_string(&command)
                .expect("命令该能序列化");
            let back: RemoteCommand =
                serde_json::from_str(&text)
                    .expect("命令该能解回来");
            assert_eq!(back, command);
        }
    }

    /// 标签用 snake_case,且写在 `type` 字段里 —— 钉死一个例子,
    /// 免得哪天换了 derive 的属性没人发现。
    #[test]
    fn a_command_is_tagged_in_snake_case() {
        let text =
            serde_json::to_value(&RemoteCommand::Seek {
                ms: 1_000,
            })
            .expect("命令该能序列化");

        assert_eq!(
            text,
            json!({"type": "seek", "ms": 1000})
        );
    }

    /// 上报的整份状态能原样解回来,含队列。
    ///
    /// 队列归被控端(见 `docs/adr/0030`),遥控器显示的就是这一份 ——
    /// 掉了它,手机上的列表会是空的而 pc1 照常续播。
    #[test]
    fn a_report_round_trips_with_its_queue() {
        let report = RemoteStateDto {
            track: Some(track("1")),
            position_ms: 12_345,
            state: RemotePlayState::Playing,
            queue: vec![track("1"), track("2")],
            queue_index: 0,
            volume: 0.8,
            sent_at: 1_700_000_000_000,
        };

        let text = serde_json::to_string(&report)
            .expect("上报该能序列化");
        let back: RemoteStateDto =
            serde_json::from_str(&text)
                .expect("上报该能解回来");

        assert_eq!(back, report);
    }

    /// 什么都没放时 `track` 是 `None`,不是一首空歌。
    ///
    /// 用空 `TrackDto` 冒充的话,遥控器会画出一行没有名字的歌,
    /// 而那和「这首歌的标题拿不到」看起来一模一样。
    #[test]
    fn an_idle_report_has_no_track() {
        let report = RemoteStateDto {
            track: None,
            position_ms: 0,
            state: RemotePlayState::Idle,
            queue: Vec::new(),
            queue_index: 0,
            volume: 1.0,
            sent_at: 1,
        };

        let back: RemoteStateDto = serde_json::from_str(
            &serde_json::to_string(&report)
                .expect("上报该能序列化"),
        )
        .expect("空闲上报该能解回来");

        assert_eq!(back.track, None);
        assert_eq!(back.state, RemotePlayState::Idle);
    }

    /// 遥控相关的每一条上行消息都走 `ClientSignal` 自己那套标签。
    #[test]
    fn remote_client_signals_round_trip() {
        let messages = [
            ClientSignal::ClaimControl {
                target: "pc1".to_owned(),
                resume: None,
            },
            ClientSignal::ClaimControl {
                target: "pc1".to_owned(),
                resume: Some(4),
            },
            ClientSignal::ExitControlled,
            ClientSignal::Command {
                to: "pc1".to_owned(),
                cmd: RemoteCommand::Next,
            },
            ClientSignal::State {
                state: RemoteStateDto {
                    track: None,
                    position_ms: 0,
                    state: RemotePlayState::Paused,
                    queue: Vec::new(),
                    queue_index: 0,
                    volume: 0.5,
                    sent_at: 7,
                },
            },
            ClientSignal::SnapshotRequest {
                to: "pc1".to_owned(),
            },
        ];

        for message in messages {
            let back: ClientSignal = serde_json::from_str(
                &serde_json::to_string(&message)
                    .expect("消息该能序列化"),
            )
            .expect("消息该能解回来");
            assert_eq!(back, message);
        }
    }

    /// 下行同理。
    #[test]
    fn remote_server_signals_round_trip() {
        let device = DeviceDto {
            id: "phone".to_owned(),
            name: "小米13".to_owned(),
        };
        let messages = [
            ServerSignal::ControlGranted { generation: 3 },
            ServerSignal::ControlRevoked {
                by: "another".to_owned(),
            },
            ServerSignal::Command {
                cmd: RemoteCommand::Pause,
            },
            ServerSignal::State {
                from: "pc1".to_owned(),
                state: RemoteStateDto {
                    track: None,
                    position_ms: 0,
                    state: RemotePlayState::Buffering,
                    queue: Vec::new(),
                    queue_index: 0,
                    volume: 1.0,
                    sent_at: 9,
                },
            },
            ServerSignal::SnapshotRequest,
            ServerSignal::ControlledBy { device },
        ];

        for message in messages {
            let back: ServerSignal = serde_json::from_str(
                &serde_json::to_string(&message)
                    .expect("消息该能序列化"),
            )
            .expect("消息该能解回来");
            assert_eq!(back, message);
        }
    }
}
