//! 成员这一侧看服务端的全局组状态(#142):本机在组里是什么身份,此刻该怎么出声。
//!
//! 状态只由服务端写(`server::syncplay::group`),这里只做三件事:收下更新的那一版、
//! 认出本机是独奏 / 只当遥控器 / 出声设备、把状态换算成「这一刻该放哪一条的哪个位置」。
//! 只有规则:时间由调用方传进来,不碰播放器、不发网络。
//!
//! 掉线规则(用户 2026-09-26):出声设备与服务端断开就立刻暂停自己,不按旧状态往下放;
//! 重连拿到最新状态再照着放。

use contract::{GroupStateDto, TrackDto};

/// 此刻该放的那一条,按全局状态换算好了(时刻都在服务端时钟上)。
#[derive(Debug, Clone, PartialEq)]
pub struct Effective {
    pub clock_epoch: u64,
    pub queue_id: i64,
    pub revision: i64,
    pub entry_id: i64,
    pub track: TrackDto,
    /// 服务端时钟 `anchor_us` 这一刻,媒体该在第 `position_us` 微秒。
    pub anchor_us: u64,
    pub position_us: u64,
    pub playing: bool,
    /// 一起开始的那一刻,之前不出声。
    pub start_us: u64,
}

/// 本机在组里的身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// 不在组里:本机放、本机队列,行为与没有组时一样。
    Solo,
    /// 在组里、不出声:只当遥控器,本机播放器停着。
    Remote,
    /// 在组里、出声:照全局状态对准本机播放。
    Output,
}

/// 此刻本机该怎么出声。
#[derive(Debug, Clone, PartialEq)]
pub enum Sound {
    /// 不在组里:照本机自己的放。
    Solo,
    /// 在组里但本机不出声:停着。
    Silent,
    /// 出声设备,却没有可跟的(连不上服务端、组里还没有歌):停在当前位置,不往下放。
    Hold,
    /// 照这一条放。装箱:它比其余几种大两百多字节,每拍都要搬。
    Follow(Box<Effective>),
}

/// 本机手上的那份全局状态。
#[derive(Debug)]
pub struct GlobalGroup {
    me: String,
    state: Option<GroupStateDto>,
    online: bool,
}

impl GlobalGroup {
    pub fn new(me: impl Into<String>) -> Self {
        Self {
            me: me.into(),
            state: None,
            online: false,
        }
    }

    /// 服务端推来(或意图的应答带回)的一版。比手上旧的丢掉 —— 应答与广播可能乱序到。
    /// `None` 是组散了。返回有没有换。
    pub fn on_state(
        &mut self,
        state: Option<GroupStateDto>,
    ) -> bool {
        match (&self.state, state) {
            (Some(held), Some(state))
                if state.version <= held.version =>
            {
                false
            }
            (_, state) => {
                self.state = state;
                true
            }
        }
    }

    /// 信令连上 / 断开。断开时出声设备立刻停下(掉线规则)。
    pub fn set_online(&mut self, online: bool) {
        self.online = online;
    }

    pub fn is_online(&self) -> bool {
        self.online
    }

    pub fn state(&self) -> Option<&GroupStateDto> {
        self.state.as_ref()
    }

    pub fn standing(&self) -> Standing {
        let Some(state) = &self.state else {
            return Standing::Solo;
        };
        if !state.members.contains(&self.me) {
            Standing::Solo
        } else if state.outputs.contains(&self.me) {
            Standing::Output
        } else {
            Standing::Remote
        }
    }

    /// 本机在组里(出声或只当遥控器)。点歌入口据此决定发意图还是本机放。
    pub fn is_member(&self) -> bool {
        self.standing() != Standing::Solo
    }

    /// 服务端时钟 `now_us` 这一刻,本机该怎么出声。
    pub fn sound(&self, now_us: u64) -> Sound {
        match self.standing() {
            Standing::Solo => Sound::Solo,
            Standing::Remote => Sound::Silent,
            Standing::Output if !self.online => Sound::Hold,
            Standing::Output => self
                .effective(now_us)
                .map_or(Sound::Hold, |now| {
                    Sound::Follow(Box::new(now))
                }),
        }
    }

    /// 服务端时钟 `now_us` 这一刻该放的那一条:预告的下一首到点了就是它。组里没有歌时
    /// 是 `None`。控制条、进度也读它。
    pub fn effective(
        &self,
        now_us: u64,
    ) -> Option<Effective> {
        let state = self.state.as_ref()?;
        let now = state.now.as_ref()?;
        let current = Effective {
            clock_epoch: state.clock_epoch,
            queue_id: now.queue_id,
            revision: now.revision,
            entry_id: now.entry_id,
            track: now.track.clone(),
            anchor_us: now.anchor_us,
            position_us: now.position_us,
            playing: now.playing,
            start_us: now.anchor_us,
        };
        Some(match &now.next {
            Some(next)
                if now.playing && now_us >= next.at_us =>
            {
                Effective {
                    entry_id: next.entry_id,
                    track: next.track.clone(),
                    anchor_us: next.at_us,
                    position_us: 0,
                    start_us: next.at_us,
                    ..current
                }
            }
            _ => current,
        })
    }

    /// 组里其余出声设备的 id(「与 X、Y 一起播放」)。
    pub fn fellow_outputs(&self) -> Vec<String> {
        self.state
            .as_ref()
            .map(|state| {
                state
                    .outputs
                    .iter()
                    .filter(|id| **id != self.me)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use contract::{
        GroupNowDto, LoopModeDto, NextEntryDto, TrackDto,
    };

    use super::*;

    fn track(id: &str) -> TrackDto {
        TrackDto {
            platform: "netease".to_owned(),
            id: id.to_owned(),
            title: id.to_owned(),
            alias: None,
            artists: vec![],
            cover: None,
            duration_ms: 100_000,
        }
    }

    fn state(
        version: u64,
        outputs: &[&str],
    ) -> GroupStateDto {
        GroupStateDto {
            version,
            members: vec![
                "phone".to_owned(),
                "pc".to_owned(),
            ],
            outputs: outputs
                .iter()
                .map(|id| (*id).to_owned())
                .collect(),
            clock_epoch: 9,
            now: Some(GroupNowDto {
                queue_id: 7,
                revision: 1,
                entry_id: 1,
                track: track("a"),
                anchor_us: 1_000_000,
                position_us: 0,
                playing: true,
                next: Some(NextEntryDto {
                    entry_id: 2,
                    track: track("b"),
                    at_us: 101_000_000,
                }),
                shuffled: false,
                loop_mode: LoopModeDto::Off,
            }),
        }
    }

    fn pc(version: u64) -> GlobalGroup {
        let mut group = GlobalGroup::new("pc");
        group.on_state(Some(state(version, &["pc"])));
        group.set_online(true);
        group
    }

    /// 成员与出声设备是两个集合:出声的跟着放,只当遥控器的停着,组外的独奏。
    #[test]
    fn members_and_outputs_decide_how_this_device_sounds() {
        assert_eq!(pc(1).standing(), Standing::Output);

        let mut phone = GlobalGroup::new("phone");
        phone.on_state(Some(state(1, &["pc"])));
        assert_eq!(phone.standing(), Standing::Remote);
        assert_eq!(phone.sound(0), Sound::Silent);
        assert!(phone.is_member());

        let mut tablet = GlobalGroup::new("tablet");
        tablet.on_state(Some(state(1, &["pc"])));
        assert_eq!(tablet.sound(0), Sound::Solo);
        assert!(!tablet.is_member());
    }

    /// 出声设备与服务端断开就停下,不按旧状态往下放(掉线规则)。
    #[test]
    fn an_offline_output_holds() {
        let mut group = pc(1);

        group.set_online(false);

        assert_eq!(group.sound(50_000_000), Sound::Hold);
    }

    /// 比手上旧的一版丢掉;组散了就回到独奏。
    #[test]
    fn older_versions_are_ignored_and_dissolving_goes_solo()
    {
        let mut group = pc(5);

        assert!(!group.on_state(Some(state(4, &[]))));
        assert_eq!(group.standing(), Standing::Output);

        assert!(group.on_state(None));
        assert_eq!(group.standing(), Standing::Solo);
    }

    /// 预告的下一首到点就换过去,锚在它开始的那一刻、从头放。
    #[test]
    fn the_next_entry_takes_over_at_its_moment() {
        let group = pc(1);

        let before = group.effective(100_000_000).unwrap();
        assert_eq!(before.entry_id, 1);
        let after = group.effective(101_000_000).unwrap();
        assert_eq!(after.entry_id, 2);
        assert_eq!(after.anchor_us, 101_000_000);
        assert_eq!(after.position_us, 0);
        assert!(matches!(
            group.sound(101_000_000),
            Sound::Follow(now) if now.entry_id == 2
        ));
    }

    /// 「与 X、Y 一起播放」只列别的出声设备。
    #[test]
    fn fellow_outputs_leave_this_device_out() {
        let mut group = GlobalGroup::new("pc");
        group.on_state(Some(state(1, &["pc", "phone"])));

        assert_eq!(group.fellow_outputs(), vec!["phone"]);
    }
}
