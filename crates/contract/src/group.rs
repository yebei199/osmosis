//! 播放组的共同计划与校时(#137 ⑤)。
//!
//! 多台设备一起出声靠的是**同一条时间线**:主端(组里持有时间线、决定下一首的那一台)发布
//! 「服务端时钟 T 这一刻，媒体该在 P」,每台成员把它换算到自己的单调时钟上照着放。
//! 时刻一律用**服务端**的单调时钟：各端各自与服务端校时(多次往返，取最短的那几次),
//! 从不拿两台设备的时钟读数直接相减。
//!
//! 服务端不解释计划(`docs/adr/0030`),只认「是不是当前主端、当前任期发的」,然后原样转发。

use serde::{Deserialize, Serialize};

use crate::TrackDto;

/// 循环模式。随计划下发：主端交接时新主端照着接着放，不按自己的开关另起一套。
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
/// 提前发，不等这一首放完：成员有时间先把它备好，切歌那一刻各自在同一个时刻换过去;
/// 主端失联时，已经预告的这一首也是成员「放完已确认的有限计划」的一部分。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct NextEntryDto {
    pub entry_id: i64,
    pub track: TrackDto,
    pub at_us: u64,
}

/// 主端发布的共同计划。
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize,
)]
pub struct GroupPlanDto {
    /// 主端在本任期里发的第几份，成员只认比手上新的。任期本身随转发一起到(见
    /// [`crate::ServerSignal::GroupPlan`]),两者合起来才是计划的身份。
    pub seq: u64,
    /// 服务端时钟的纪元，服务端每次启动换一个。计划里的时刻都在这个钟上;纪元对不上的计划
    /// 连同它的起播时刻、下一首预告一起作废。
    pub clock_epoch: u64,
    /// 放的是服务端哪个队列的哪一版的哪一条。成员按标识自己取执行副本(`docs/adr/0031`)。
    pub queue_id: i64,
    pub revision: i64,
    pub entry_id: i64,
    /// 这一条是哪首歌，也是媒体身份的一半(平台、id、时长)。成员取到的媒体对不上、
    /// 或者跳不到要的位置，就报这一次加入或操作失败，不自己从头放。
    pub track: TrackDto,
    /// 锚点：服务端时钟 `anchor_us` 这一刻，媒体该在第 `position_us` 微秒。
    pub anchor_us: u64,
    pub position_us: u64,
    /// 暂停着就停在锚点那个位置。
    pub playing: bool,
    /// 一起起播的那一刻。之前谁都不出声;准备得慢的成员过了这一刻再追进来。
    pub start_us: u64,
    pub next: Option<NextEntryDto>,
    /// 这份计划管到哪一刻为止。主端失联时，成员放到这里就停下、显示异常，不自己往下编。
    pub valid_until_us: u64,
    /// 播放次序(条目号)。只在变了的时候带(一轮新洗、新任期的第一份);成员记住最近那一份。
    #[serde(default)]
    pub play_order: Option<Vec<i64>>,
    #[serde(default)]
    pub round: u64,
    #[serde(default)]
    pub shuffled: bool,
    #[serde(default)]
    pub loop_mode: LoopModeDto,
}

impl GroupPlanDto {
    /// 服务端时钟 `at_us` 这一刻媒体该在哪(微秒)。暂停或还没起播给 `None`。
    pub fn position_at(&self, at_us: u64) -> Option<u64> {
        if !self.playing || at_us < self.start_us {
            return None;
        }
        let elapsed = at_us as i64 - self.anchor_us as i64;
        Some((self.position_us as i64 + elapsed).max(0) as u64)
    }
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

    fn plan() -> GroupPlanDto {
        GroupPlanDto {
            seq: 1,
            clock_epoch: 7,
            queue_id: 3,
            revision: 2,
            entry_id: 11,
            track: track(),
            anchor_us: 10_000_000,
            position_us: 5_000_000,
            playing: true,
            start_us: 10_000_000,
            next: None,
            valid_until_us: 240_000_000,
            play_order: None,
            round: 0,
            shuffled: false,
            loop_mode: LoopModeDto::Off,
        }
    }

    /// 起播之后按锚点线性往前;起播之前、暂停着都没有「该在哪」。
    #[test]
    fn a_plan_says_where_the_media_should_be() {
        let plan = plan();
        assert_eq!(plan.position_at(12_000_000), Some(7_000_000));
        assert_eq!(plan.position_at(9_000_000), None, "还没起播");
        let paused = GroupPlanDto { playing: false, ..plan };
        assert_eq!(paused.position_at(12_000_000), None);
    }

    /// 老报文没有次序与循环字段也解得出来(新增字段都有缺省)。
    #[test]
    fn optional_fields_default_when_absent() {
        let mut value = serde_json::to_value(plan()).unwrap();
        for key in ["play_order", "round", "shuffled", "loop_mode"] {
            value.as_object_mut().unwrap().remove(key);
        }
        let back: GroupPlanDto = serde_json::from_value(value).unwrap();
        assert_eq!(back.loop_mode, LoopModeDto::Off);
        assert_eq!(back.play_order, None);
    }
}
