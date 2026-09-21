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
    /// 播服务端上那个队列的这一条。
    ///
    /// **只带标识,不带曲目**(`docs/adr/0031`)。从前这里拖着整批
    /// `Vec<TrackDto>`,977 首序列化 22 万字节,是信令上限的三倍多,
    /// 而超限不是丢一条消息、是整条连接断掉(见 [`crate::MAX_SIGNAL_BYTES`])。
    /// 曲目数据现在走 HTTP,被控端按 `queue_id`/`revision` 自己去取。
    ///
    /// 自动续播仍然在被控端发生:它取到的是**整个**执行副本,遥控器锁屏、
    /// 断线都不该让 pc1 停在一首上 —— 这一条与改之前一样,变的只是那批歌
    /// 从哪条路过来。
    Play {
        queue_id: i64,
        /// 要播的是哪一版。带上它,被控端才知道自己手上那份够不够新。
        revision: i64,
        /// 队列里的哪一条。**不是下标** —— 队列允许同一首歌出现多次,
        /// 而下标会随插入删除整体挪位。
        entry_id: i64,
        /// 这一次操作的标识,由遥控器生成。
        ///
        /// 重试同一次点播不该再次重置播放,连点 A、B 时迟到的 A 也不能覆盖
        /// B —— 两件事都靠比对它(`docs/adr/0031` 七)。
        operation_id: String,
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
    /// `Play` 报的是队列标识那一组:一次点播在日志里要能与服务端那边的
    /// 队列版本、以及后面那条执行报告对上,而 `operation_id` 正是把重试、
    /// 连点与迟到三种情形区分开的那个键。
    pub fn summary(&self) -> String {
        match self {
            Self::Play {
                queue_id,
                revision,
                entry_id,
                operation_id,
            } => format!(
                "play(队列 {queue_id}@{revision}, 条目 {entry_id}, 操作 {operation_id})"
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

/// 被控端每秒上报一次的**小状态**。
///
/// **进度靠上报,不靠遥控器推算**(`docs/adr/0030`):各设备网速不同,
/// 「点了播」到「真出声」之间的延迟不可知,遥控器推算出来的位置必然与
/// 音箱里的声音对不上,而且不会有任何报错。
///
/// 「小」是本轮的重点:这里**没有一个字段随用户的数据增长**
/// (`docs/adr/0031` 三)。从前它带着被控端的整个队列,977 首时每秒往连接上
/// 打 23 万字节,而信令上限是 64 KiB —— 超限在服务端那侧是跳出读循环、
/// 整条连接断掉,于是被控端每秒把自己踢下线一次(#109 F-002)。队列本身
/// 现在按 `queue_id`/`revision` 经 HTTP 取,变了才取一次。
///
/// 当前曲目仍然整条带着:它是**一首**,大小有界,而少了它遥控器在拉到队列
/// 之前连歌名都显示不出来 —— 状态新鲜度与队列加载状态是两件事。
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
    /// 被控端的音量,0..=1 —— 遥控器上的滑块读的是这个数。
    pub volume: f32,

    /// 被控端手上那个队列在服务端的 id。
    ///
    /// `None` 是「这个队列还没同步上去」:服务端不可达时本机照常起播,
    /// 只是标成未同步(`docs/adr/0031` 八)。遥控器见到 `None` 就知道
    /// 自己拉不到列表,而不是拉了个空的。
    pub queue_id: Option<i64>,
    /// 被控端所知的、服务端上已提交的那一版。
    pub revision: Option<i64>,
    /// 被控端**实际应用**的那一版。
    ///
    /// 与 [`Self::revision`] 分开:两者不等就是「新版本待应用」,界面要标出来。
    /// 合成一个的话,下载失败时被控端只能在「谎报已应用」和「谎报没收到」
    /// 之间挑一个(`docs/adr/0031` 一)。
    pub applied_revision: Option<i64>,
    /// 正在放的是队列里的哪一条。下标会随插入删除挪位,`entry_id` 不会。
    pub entry_id: Option<i64>,
    /// 队列一共几首。遥控器拉列表之前先拿它画「共 N 首」。
    pub queue_len: u32,

    /// 执行会话标识:被控端进程启动时的毫秒挂钟,重启换一个。
    ///
    /// 与服务端 `play_queue_reports.epoch` 是同一个数 —— 同一份执行状态
    /// 走两条路(WebSocket 的小状态、HTTP 的检查点)上报,两条必须能互相
    /// 排序,各用各的序号就排不了。
    pub epoch: i64,
    /// 这一条在本次执行会话里的序号,每报一次加一。
    ///
    /// 取代了从前那个 `sent_at`:挂钟跨进程重启不可比,而「哪条更新」正是
    /// 它唯一的用处。重连之后旧连接上的残余上报仍会后到,拿它盖掉新的,
    /// 进度条就会倒退一次。过期与否仍由收信方按自己的钟判
    /// (见 `app_core::Output`)。
    pub state_seq: u64,
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

    /// 现场那一份歌单长什么样 —— 与 #108 真机量到的那一条同等规模、同等形状。
    ///
    /// 字段取真实值域:网易云的 21 位数字 id、带日文原名与英文别名的标题、
    /// 两个歌手、一条 `p2.music.126.net` 的封面直链。上面那个 `track()`
    /// 是给往返测试用的最小例子,拿它量字节会把现场少算一半。
    fn field_track(index: usize) -> TrackDto {
        TrackDto {
            platform: "netease".to_owned(),
            id: format!("{}", 2_000_000_000 + index),
            title: format!("夜に駆ける(第{index}回)"),
            alias: Some("Racing Into The Night".to_owned()),
            artists: vec![
                "YOASOBI".to_owned(),
                "Ayase".to_owned(),
            ],
            cover: Some(format!(
                "https://p2.music.126.net/{}==/1099511680000{index:05}.jpg",
                "a".repeat(22)
            )),
            duration_ms: 261_000,
        }
    }

    /// 现场规模那份歌单,长什么样。
    const FIELD_TRACKS: usize = 977;

    /// 被控端在放这一批时,它每秒那条上报的线上写法。
    ///
    /// 收整批而不是收一个长度:这个 helper 的**形状**就是本轮改的那件事 ——
    /// 从前它得把整批抄进 `queue`,现在它只抄得动一个长度与当前那一首。
    /// 下面两条断言一个字没改,变的只有它。
    fn report_for(tracks: &[TrackDto]) -> ClientSignal {
        ClientSignal::State {
            state: RemoteStateDto {
                track: tracks.first().cloned(),
                position_ms: 42_000,
                state: RemotePlayState::Playing,
                volume: 0.8,
                queue_id: Some(7),
                revision: Some(3),
                applied_revision: Some(3),
                entry_id: Some(12),
                queue_len: tracks.len() as u32,
                epoch: 1_700_000_000_000,
                state_seq: 42,
            },
        }
    }

    /// 在这一批上点第一首时,发出去的那条命令的线上写法。
    fn command_for(tracks: &[TrackDto]) -> ClientSignal {
        ClientSignal::Command {
            to: "pc1".to_owned(),
            cmd: RemoteCommand::Play {
                queue_id: 7,
                revision: 3,
                entry_id: tracks.first().map_or(0, |_| 12),
                operation_id: "8f1c2e0a-play".to_owned(),
            },
        }
    }

    /// 一份最小的小状态,只把要断言的那几样填上。
    ///
    /// 有了它,往 `RemoteStateDto` 里加一个标量字段不必回来改六处 fixture ——
    /// 而那六处每一处都只关心其中一两个字段。
    fn idle_state() -> RemoteStateDto {
        RemoteStateDto {
            track: None,
            position_ms: 0,
            state: RemotePlayState::Idle,
            volume: 1.0,
            queue_id: None,
            revision: None,
            applied_revision: None,
            entry_id: None,
            queue_len: 0,
            epoch: 1_700_000_000_000,
            state_seq: 1,
        }
    }

    fn wire_len(message: &ClientSignal) -> usize {
        serde_json::to_string(message)
            .expect("消息该能序列化")
            .len()
    }

    /// **两个方向的字节数都不随队列长度增长**(AC-2)。
    ///
    /// 这一条以前是反的,而且两个方向现在都有**现场实测**(#109 Checklist 1)。
    ///
    /// 2026-09-21 拿用户真实的「我喜欢的」(977 首,`/liked` 响应体 224142
    /// 字节)按两版契约各序列化一次:
    ///
    /// | 方向 | 协议 2 | 协议 3 | 对 65536 |
    /// |---|---|---|---|
    /// | 出站 `Command{Play}` | 215989 | 187 | 3.3 倍 → 0.3% |
    /// | 入站 `State` | **216260** | **407** | 3.3 倍 → 0.6% |
    ///
    /// 入站那个数此前只是推算(#109 F-002 标着「推算不是实测」),现在是
    /// 实测。出站 215989 与 #108 真机上量到的 224194 差 3.8% —— 同一份歌单
    /// 在两个时刻的元数据不完全一样,量级与结论都对得上。
    ///
    /// 队列挪进服务端之后(`docs/adr/0031`),命令只带
    /// `queue_id/revision/entry_id/operation_id`,上报只带长度与当前那一首,
    /// 两条都是定长的。曲目数据走 HTTP。
    ///
    /// 断言用相等而不是「小于某个数」:要钉的是**这条线是平的**,而不是
    /// 「它现在还够低」。哪天有人往上报里塞回一个随用户数据增长的字段,
    /// 这一条当场红 —— 而 F-002 那个洞正是这么来的。
    #[test]
    fn neither_direction_grows_with_the_queue() {
        let one = [field_track(0)];
        let hundred: Vec<TrackDto> =
            (0..100).map(field_track).collect();
        let field: Vec<TrackDto> =
            (0..FIELD_TRACKS).map(field_track).collect();

        // 一百首与九百七十七首**逐字节相等**。两者的 `queue_len` 都是三位数,
        // 所以这一对之间已经没有任何随数据变化的东西 —— 平线就是平线。
        assert_eq!(
            wire_len(&report_for(&hundred)),
            wire_len(&report_for(&field)),
            "上报的字节数不该随队列长度增长"
        );
        assert_eq!(
            wire_len(&command_for(&hundred)),
            wire_len(&command_for(&field)),
            "点播命令的字节数不该随队列长度增长"
        );

        // 一首与九百七十七首之间只差 `queue_len` 那几位十进制数字 ——
        // 这是 log 而不是线性,而且上限是 u32 的十位。放宽到 16 字节,
        // 塞回任何一个随曲目数增长的字段都会把它撑爆好几个数量级。
        let spread = wire_len(&report_for(&field))
            .abs_diff(wire_len(&report_for(&one)));
        assert!(
            spread <= 16,
            "一首与 {FIELD_TRACKS} 首之间差了 {spread} 字节,\
             只该差队列长度那几位数字"
        );
    }

    /// 而且离上限很远 —— 不是「刚好挤进去」。
    ///
    /// 上一条只说这条线是平的,平在 65535 上同样算平。这一条说的是它平在
    /// 哪:两条都在上限的百分之一以内(现场那 977 首实测 407 字节,
    /// 0.6%),于是以后往小状态里加一两个标量字段不必每次重新量。
    #[test]
    fn a_field_sized_queue_now_fits_far_inside_the_limit() {
        let field: Vec<TrackDto> =
            (0..FIELD_TRACKS).map(field_track).collect();

        let inbound = wire_len(&report_for(&field));
        let outbound = wire_len(&command_for(&field));

        assert!(
            inbound < crate::MAX_SIGNAL_BYTES / 100,
            "上报 {inbound} 字节,该远在上限 {} 之内",
            crate::MAX_SIGNAL_BYTES
        );
        assert!(
            outbound < crate::MAX_SIGNAL_BYTES / 100,
            "命令 {outbound} 字节,该远在上限 {} 之内",
            crate::MAX_SIGNAL_BYTES
        );
    }

    /// 命令的线上写法就是契约本身:两端各自按 `type` 分支。
    ///
    /// 改一个标签名等于换一条协议,而症状是「按了没反应」—— 对端解不出来时
    /// 静默丢弃(见 `server::syncplay::signaling::dispatch`),不会有任何报错。
    #[test]
    fn every_command_round_trips_through_its_tag() {
        let commands = [
            RemoteCommand::Play {
                queue_id: 7,
                revision: 3,
                entry_id: 12,
                operation_id: "op-1".to_owned(),
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
    /// 队列本身不在上报里了(`docs/adr/0031`),但**指向它的那几个标识在** ——
    /// 掉了任何一个,遥控器就不知道该去 HTTP 上取哪一版,列表会一直空着
    /// 而 pc1 照常续播。
    #[test]
    fn a_report_round_trips_with_its_queue_identity() {
        let report = RemoteStateDto {
            track: Some(track("1")),
            position_ms: 12_345,
            state: RemotePlayState::Playing,
            volume: 0.8,
            queue_id: Some(7),
            revision: Some(4),
            applied_revision: Some(3),
            entry_id: Some(12),
            queue_len: 2,
            ..idle_state()
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
        let report = idle_state();

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
                    state: RemotePlayState::Paused,
                    volume: 0.5,
                    ..idle_state()
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
                state: Box::new(RemoteStateDto {
                    state: RemotePlayState::Buffering,
                    ..idle_state()
                }),
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
