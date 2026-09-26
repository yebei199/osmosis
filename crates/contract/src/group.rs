//! 播放组的全局状态、意图与校时(#142)。
//!
//! 多台设备一起出声靠的是**同一条时间线**:服务端持有组的全局状态,写的是「服务端时钟 T
//! 这一刻,媒体该在 P」,每台出声设备把它换算到自己的单调时钟上照着放。时刻一律用**服务端**
//! 的单调时钟:各端各自与服务端校时(多次往返,取最短的那几次),从不拿两台设备的时钟读数
//! 直接相减。组里设备对等,任何一台的点歌、切歌、暂停、拖动都是发给服务端的意图。

use serde::{Deserialize, Serialize};

use crate::TrackDto;

/// 循环模式,全局状态的一部分。
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum LoopModeDto {
    #[default]
    Off,
    All,
    One,
}

/// 预告的下一首：服务端时钟 `at_us` 那一刻从它的开头放起。
///
/// 提前发,不等这一首放完:出声设备各自在同一个时刻换过去,不必等服务端的下一次广播。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct NextEntryDto {
    pub entry_id: i64,
    pub track: TrackDto,
    pub at_us: u64,
}

/// 组的全局播放状态(#142):服务端持有的唯一一份,版本一变就广播给账号下每台在线设备。
///
/// 不再有主端:任何成员的点歌、切歌、暂停、拖动都是发给服务端的意图(HTTP,
/// `/group/*`),服务端定序、改写、加版本号。出声的设备照它对准本机播放。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct GroupStateDto {
    /// 每应用一条意图加一,落库,服务端重启也不回退。只收比手上新的。
    pub version: u64,
    /// 加入了组、能控制的设备。手机只当遥控器 = 在这里、不在 `outputs` 里。
    pub members: Vec<String>,
    /// 成员里真正出声的那几台(`outputs ⊆ members`)。
    pub outputs: Vec<String>,
    /// 下面那些时刻所在的服务端时钟纪元(服务端每次启动换一个)。
    pub clock_epoch: u64,
    /// 此刻放的是什么、放到哪。组里还没有歌时是 `None`。
    pub now: Option<GroupNowDto>,
}

/// 全局状态里「此刻放的是什么」。时刻都在服务端单调时钟上(见 [`GroupStateDto::clock_epoch`])。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct GroupNowDto {
    /// 服务端哪个队列的哪一版的哪一条。出声设备按标识自己取执行副本(`docs/adr/0031`)。
    pub queue_id: i64,
    pub revision: i64,
    pub entry_id: i64,
    pub track: TrackDto,
    /// 服务端时钟 `anchor_us` 这一刻,媒体在第 `position_us` 微秒。播放时锚点可以在
    /// 未来(一起起播的那一刻),之前谁都不出声。
    pub anchor_us: u64,
    pub position_us: u64,
    pub playing: bool,
    /// 放完这一首接哪一首、在哪一刻换。播放时才有;单曲循环时就是它自己。
    pub next: Option<NextEntryDto>,
    pub shuffled: bool,
    pub loop_mode: LoopModeDto,
}

impl GroupNowDto {
    /// 服务端时钟 `at_us` 这一刻媒体该在哪(微秒)。暂停或还没到锚点时就是锚点那个位置。
    pub fn position_at(&self, at_us: u64) -> u64 {
        if !self.playing || at_us < self.anchor_us {
            return self.position_us;
        }
        self.position_us + (at_us - self.anchor_us)
    }
}

/// `POST /group/play`:点歌。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct GroupPlayDto {
    /// 发意图的那台。不在组里就被拒:组外设备点歌走本机。
    pub device_id: String,
    pub pick: GroupPickDto,
}

/// 点的是什么。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroupPickDto {
    /// 歌单、卡墙、搜索:用户眼前这一批,点的是第 `index` 首。服务端把它冻成组队列的
    /// 新一版(同一批不重复发布),再切过去。
    Tracks { tracks: Vec<TrackDto>, index: usize },
    /// 队列页:切到组队列里已有的一条。
    Entry {
        queue_id: i64,
        revision: i64,
        entry_id: i64,
    },
}

/// `POST /group/transport`:控制条与系统媒体控件。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct GroupTransportDto {
    pub device_id: String,
    pub op: TransportOpDto,
}

/// 控制条上的一下。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum TransportOpDto {
    Pause,
    Resume,
    Next,
    Prev,
    Seek { position_ms: u64 },
    Shuffle { on: bool },
    Loop { mode: LoopModeDto },
}

/// `POST /group/outputs`:改在哪几台出声;发的那台随之成为成员。
///
/// 组还没有(或还没有歌)时,`seed` 是发起的那台本机正在放的那一份:组从它接着放。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct GroupOutputsDto {
    pub device_id: String,
    pub outputs: Vec<String>,
    #[serde(default)]
    pub seed: Option<GroupSeedDto>,
}

/// 发起组的那台本机正在放的那一份。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct GroupSeedDto {
    pub queue_id: i64,
    pub revision: i64,
    pub entry_id: i64,
    pub position_ms: u64,
    pub playing: bool,
}

/// `POST /group/leave`:本机退出组,回到独奏。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct GroupLeaveDto {
    pub device_id: String,
}

/// `/group/*` 的应答:意图应用之后组的样子。组散了是 `None`。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct GroupReplyDto {
    pub state: Option<GroupStateDto>,
}

/// 出声设备每秒一条的执行事实,经服务端转给组里其他设备,只用于显示
/// (「某台没跟上」「该路由未校准」)。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct DeviceReportDto {
    /// 本机此刻放的条目。
    pub entry_id: Option<i64>,
    /// 取不到媒体、跳不到位置这类故障。好了就是 `None`。
    pub fault: Option<String>,
    pub route: Option<crate::OutputRouteDto>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> TrackDto {
        TrackDto {
            platform: "netease".to_owned(),
            id: "1".to_owned(),
            title: "歌".to_owned(),
            alias: None,
            artists: vec!["LiSA".to_owned()],
            cover: None,
            duration_ms: 234_000,
        }
    }

    /// 全局状态的位置:锚点之后线性往前,锚点之前与暂停时停在锚点那个位置。
    #[test]
    fn the_group_now_says_where_the_media_is() {
        let now = GroupNowDto {
            queue_id: 3,
            revision: 2,
            entry_id: 11,
            track: track(),
            anchor_us: 10_000_000,
            position_us: 5_000_000,
            playing: true,
            next: None,
            shuffled: false,
            loop_mode: LoopModeDto::Off,
        };
        assert_eq!(now.position_at(12_000_000), 7_000_000);
        assert_eq!(now.position_at(9_000_000), 5_000_000);
        let paused = GroupNowDto {
            playing: false,
            ..now
        };
        assert_eq!(
            paused.position_at(12_000_000),
            5_000_000
        );
    }
}
