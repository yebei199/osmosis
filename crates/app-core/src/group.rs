//! 播放组，成员这一侧(#137 ⑤):手上的组、最近那份共同计划，以及此刻该怎么出声。
//!
//! 主端(持有组时间线、决定下一首的那一台)写计划：计划说「服务端时钟 T 这一刻媒体该在 P」。
//! 跟随端把它换算到本机时钟上，由音频层的跟随器贴着走(`audio::sync`)。主端本机照常自由地放，
//! 计划是它实际播放的镜像，只在偏离超过 [`REANCHOR_US`] 时重新定锚(见 [`Cue`]);一起开始时
//! 锚在稍后的一刻，主端自己也照这一刻起播。
//!
//! 身份与有效期(不变量,`docs/adr/0030`):
//!
//! - 只收当前任期或更新任期的计划;同一任期只收序号更大的 —— 旧任期、旧序号的迟到计划不生效;
//! - 计划的纪元(服务端时钟的那一次启动)对不上本机校时的纪元，就换算不了，不出声
//!   (换算在调用方，见 [`Effective::clock_epoch`]);
//! - 主端失联时**不**另选：照已确认的计划放到有效期末尾(含已经预告的下一首),然后停下，
//!   显示异常。
//!
//! 与 `session.rs` 一样只有规则：时间由调用方传进来，不碰播放器、不发网络。

use contract::{
    GroupPlanDto, LoopModeDto, NextEntryDto, TrackDto,
};

/// 起播、跳转、恢复时，计划的锚点定在「现在」之后多久。
///
/// 这段时间让计划先到各台成员手上：经服务端转一跳，局域网里几十毫秒，手机 WiFi 偶尔上百。
/// 大家都在锚点那一刻一起开始，而不是主端先响、跟随端静音追上来。
pub const LEAD_US: u64 = 500_000;

/// 当前这一首还剩多久时预告下一首。
///
/// 预告让成员提前备好下一首、到点各自换过去;主端失联时，已经预告的这一首也在「已确认的
/// 有限计划」里。十五秒够手机取直链、开流(真机一两秒,#121)。
pub const PREANNOUNCE_US: u64 = 15_000_000;

/// 主端多久没有计划(心跳一秒一次)算失联。三次心跳没到：漏一次是抖动，漏三次是真的断了
/// (同 `output::STALE_AFTER_MS` 的道理)。
pub const MASTER_SILENT_MS: u64 = 3_000;

/// 服务端通告的组。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Membership {
    term: u64,
    master: Option<String>,
    members: Vec<String>,
}

/// 本机在组里是什么身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupRole {
    /// 不在多成员组里：照本机自己的放。
    Solo,
    /// 写计划的那一台。
    Master,
    /// 照主端的计划放。
    Follower,
}

/// 此刻该放的那一条，按计划换算好了(时刻都在服务端时钟上)。
#[derive(Debug, Clone, PartialEq)]
pub struct Effective {
    pub clock_epoch: u64,
    pub queue_id: i64,
    pub revision: i64,
    pub entry_id: i64,
    pub track: TrackDto,
    /// 服务端时钟 `anchor_us` 这一刻，媒体该在第 `position_us` 微秒。
    pub anchor_us: u64,
    pub position_us: u64,
    pub playing: bool,
    /// 一起开始的那一刻，之前不出声。
    pub start_us: u64,
}

/// 此刻该怎么出声。
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// 不在多成员组里。
    Solo,
    /// 在组里，还没拿到计划：别出声。
    Waiting,
    /// 照这一条放。
    Follow(Effective),
    /// 主端失联、已确认的计划也放到头了：停下，显示异常。
    Expired,
}

/// 主端写计划时，本机播放的样子(不含时刻 —— 时刻由 [`Cue`] 与现在的时间定)。
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub clock_epoch: u64,
    pub queue_id: i64,
    pub revision: i64,
    pub entry_id: i64,
    pub track: TrackDto,
    /// 下一首是哪一条(队列里的下一首;单曲循环时是它自己)。没有就是放到头了。
    pub next: Option<(i64, TrackDto)>,
    pub play_order: Option<Vec<i64>>,
    pub round: u64,
    pub shuffled: bool,
    pub loop_mode: LoopModeDto,
}

/// 这一份计划的时间线怎么定。
///
/// 主端本机照常自由地放(暂停、跳转、切歌都走原来那条路),计划是它实际播放的镜像:
/// 定时读一次「服务端时刻 T 这一刻媒体在 P」交进来。离手上那份的时间线不到 [`REANCHOR_US`]
/// 就沿用原来的锚点 —— 每次都按实测重新定锚的话，跟随端就一直在追一条带着测量抖动的线。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// 接着手上这份：心跳，或者只补上预告。
    Keep,
    /// 一起开始：锚在现在之后 [`LEAD_US`],大家(主端自己也是)在那一刻一起出声。
    Start { position_us: u64 },
    /// 本机实际在放：服务端时刻 `at_us` 这一刻，媒体在第 `position_us` 微秒。
    Playing { at_us: u64, position_us: u64 },
    /// 停在媒体的这个位置。
    Paused { position_us: u64 },
}

/// 主端实际播放偏离计划多少才重新定锚。
///
/// 两台机器的晶振差几十 ppm,主端照自己的声卡放，相对服务端时钟一分钟漂一两毫秒:两毫秒
/// 一步的重新定锚，跟随端两秒内平缓追平(±0.1% 速率修正),稳态合同是 ±5ms。门槛再低，
/// 呈现时刻本身的测量抖动就会让它不停地重新定锚。
pub const REANCHOR_US: u64 = 2_000;

/// 主端实测的时间线截距(服务端时刻 − 媒体位置),取最近几个的中位数(#137 ⑤)。
///
/// 单个呈现时刻带着输出后端的测量抖动(ALSA / AAudio 报的延迟有颗粒度),拿一个样本定锚,
/// 那一下的抖动就烙进了全组的时间线,跟随端对得再准也差着它。主端每拍交一个实测进来,
/// 拿回平滑过的「这一刻媒体在哪」。截距一下子变了很多(跳转、换歌、缓冲之后)就作废重攒。
#[derive(Debug, Default)]
pub struct Intercepts {
    samples: std::collections::VecDeque<i64>,
}

/// 攒多少个:对准一拍 200ms,三秒。
const INTERCEPT_WINDOW: usize = 15;

/// 截距变了多少算「跳了」,不是抖动。呈现时刻的抖动在毫秒级。
const INTERCEPT_JUMP_US: i64 = 20_000;

impl Intercepts {
    /// 记一个实测:服务端时刻 `at_us` 这一刻媒体在 `position_us`。返回平滑过的那一刻的位置。
    pub fn push(
        &mut self,
        at_us: u64,
        position_us: u64,
    ) -> u64 {
        let intercept = at_us as i64 - position_us as i64;
        if self.median().is_some_and(|median| {
            (intercept - median).abs() > INTERCEPT_JUMP_US
        }) {
            self.samples.clear();
        }
        self.samples.push_back(intercept);
        while self.samples.len() > INTERCEPT_WINDOW {
            self.samples.pop_front();
        }
        let median = self.median().unwrap_or(intercept);
        u64::try_from(at_us as i64 - median).unwrap_or(0)
    }

    /// 不在放了(暂停、换歌、缓冲):下一段重新攒。
    pub fn clear(&mut self) {
        self.samples.clear();
    }

    fn median(&self) -> Option<i64> {
        let mut sorted: Vec<i64> =
            self.samples.iter().copied().collect();
        sorted.sort_unstable();
        sorted.get(sorted.len() / 2).copied()
    }
}

/// 成员一侧的播放组。
#[derive(Debug)]
pub struct Group {
    me: String,
    membership: Option<Membership>,
    /// 手上最新的计划与它的任期。
    plan: Option<(u64, GroupPlanDto)>,
    /// 最近一次收到(或者自己写出)计划的本机时刻。
    heard_ms: Option<u64>,
    /// 本机真的开始跟着组放了(收到过「开始」)。之前哪怕已经在成员里(正在准备),
    /// 手上原来在放的也不动 —— 「目标原本在放的内容」要到开始那一刻才被替换。
    joined: bool,
}

impl Group {
    pub fn new(me: impl Into<String>) -> Self {
        Self {
            me: me.into(),
            membership: None,
            plan: None,
            heard_ms: None,
            joined: false,
        }
    }

    /// 本机开始跟着组放了(被叫「开始」的那一刻)。
    pub fn join(&mut self) {
        self.joined = true;
    }

    /// 服务端通告的组。本机不在成员里就是离组了：计划一并忘掉。
    pub fn on_group(
        &mut self,
        term: u64,
        master: Option<String>,
        members: Vec<String>,
        now_ms: u64,
    ) {
        if !members.contains(&self.me) {
            self.leave();
            return;
        }
        // 旧任期的通告迟到了：不理。
        if self
            .membership
            .as_ref()
            .is_some_and(|held| held.term > term)
        {
            return;
        }
        // 刚成了主端：从这一刻起算心跳，别把交接那一下判成失联。
        if master.as_deref() == Some(self.me.as_str()) {
            self.heard_ms = Some(now_ms);
        }
        self.membership = Some(Membership {
            term,
            master,
            members,
        });
    }

    /// 离开了组(被移出、不再被遥控):照本机自己的放。
    pub fn leave(&mut self) {
        self.membership = None;
        self.plan = None;
        self.heard_ms = None;
        self.joined = false;
    }

    pub fn role(&self) -> GroupRole {
        let Some(membership) = &self.membership else {
            return GroupRole::Solo;
        };
        if membership.members.len() < 2 {
            return GroupRole::Solo;
        }
        if membership.master.as_deref()
            == Some(self.me.as_str())
        {
            GroupRole::Master
        } else {
            GroupRole::Follower
        }
    }

    /// 当前任期。
    pub fn term(&self) -> Option<u64> {
        self.membership.as_ref().map(|held| held.term)
    }

    /// 手上最新的计划。
    pub fn plan(&self) -> Option<&GroupPlanDto> {
        self.plan.as_ref().map(|(_, plan)| plan)
    }

    /// 收下一份转来的计划。返回它是不是新的(要重新换算、重新对准)。
    ///
    /// 旧任期的、同任期里序号不更大的一概不收;与手上那份一模一样的是心跳，只记下「主端还在」。
    ///
    /// 播放次序只在变了的时候随计划来(整份几千条，每秒心跳都带就是白白几十 KiB):没带的，
    /// 沿用手上那一份。
    pub fn on_plan(
        &mut self,
        term: u64,
        mut plan: GroupPlanDto,
        now_ms: u64,
    ) -> bool {
        let Some(membership) = &self.membership else {
            return false;
        };
        if term < membership.term {
            return false;
        }
        if let Some((held_term, held)) = &self.plan {
            let (incoming, current) =
                ((term, plan.seq), (*held_term, held.seq));
            if incoming < current {
                return false;
            }
            if incoming == current {
                self.heard_ms = Some(now_ms);
                return false;
            }
        }
        if plan.play_order.is_none() {
            plan.play_order =
                self.plan.as_ref().and_then(|(_, held)| {
                    held.play_order.clone()
                });
        }
        self.heard_ms = Some(now_ms);
        self.plan = Some((term, plan));
        true
    }

    /// 主端多久没动静了，算不算失联。本机就是主端时从不失联。
    pub fn master_silent(&self, now_ms: u64) -> bool {
        if self.role() != GroupRole::Follower {
            return false;
        }
        self.heard_ms.is_none_or(|heard| {
            now_ms.saturating_sub(heard) > MASTER_SILENT_MS
        })
    }

    /// 服务端时钟 `now_us` 这一刻该怎么出声。
    pub fn verdict(
        &self,
        now_us: u64,
        now_ms: u64,
    ) -> Verdict {
        if self.role() == GroupRole::Solo || !self.joined {
            return Verdict::Solo;
        }
        let Some((_, plan)) = &self.plan else {
            return Verdict::Waiting;
        };
        // 有效期只在主端失联时才管用：主端在，每秒一次的心跳就在续它 —— 曲目元数据里的时长
        // 与真实媒体差上一两秒是常事，照有效期掐掉会让跟随端比主端先停。队列真放完了，主端
        // 会发一份暂停。
        if plan.playing
            && now_us >= plan.valid_until_us
            && self.master_silent(now_ms)
        {
            return Verdict::Expired;
        }
        Verdict::Follow(effective(plan, now_us))
    }

    /// 主端写一份计划。本机不是主端、或者要接着的时间线根本没有，给 `None`。
    ///
    /// 内容与手上那份一样就原样再发(心跳，序号不变);变了序号加一。交接后第一份沿用上一任主端
    /// 那份的时间线，只换任期、序号从 1 起 —— 新主端本来就在同一条时间线上，不必重新定锚。
    pub fn publish(
        &mut self,
        draft: Draft,
        cue: Cue,
        now_us: u64,
        now_ms: u64,
    ) -> Option<(u64, GroupPlanDto)> {
        if self.role() != GroupRole::Master {
            return None;
        }
        let term = self.term()?;
        let held = self.plan.as_ref();
        let line = held.and_then(|(_, held)| {
            timeline_of(held, draft.entry_id)
        });
        let (anchor_us, position_us, playing) = match cue {
            Cue::Start { position_us } => {
                (now_us + LEAD_US, position_us, true)
            }
            Cue::Paused { position_us } => match line {
                Some((anchor, at, false))
                    if at == position_us =>
                {
                    (anchor, at, false)
                }
                _ => (now_us, position_us, false),
            },
            Cue::Playing { at_us, position_us } => {
                match line {
                    Some((anchor, at, true))
                        if drift(
                            anchor,
                            at,
                            at_us,
                            position_us,
                        ) <= REANCHOR_US =>
                    {
                        (anchor, at, true)
                    }
                    _ => (at_us, position_us, true),
                }
            }
            Cue::Keep => line?,
        };
        let duration_us =
            u64::try_from(draft.track.duration_ms)
                .unwrap_or(0)
                * 1_000;
        let end_us = anchor_us
            + duration_us.saturating_sub(position_us);
        let next = draft
            .next
            .as_ref()
            .filter(|_| {
                playing
                    && end_us.saturating_sub(now_us)
                        <= PREANNOUNCE_US
            })
            .map(|(entry_id, track)| NextEntryDto {
                entry_id: *entry_id,
                track: track.clone(),
                at_us: end_us,
            });
        let valid_until_us = if playing {
            end_us
                + next.as_ref().map_or(0, |next| {
                    u64::try_from(next.track.duration_ms)
                        .unwrap_or(0)
                        * 1_000
                })
        } else {
            anchor_us
        };
        let mut plan = GroupPlanDto {
            seq: 0,
            clock_epoch: draft.clock_epoch,
            queue_id: draft.queue_id,
            revision: draft.revision,
            entry_id: draft.entry_id,
            track: draft.track,
            anchor_us,
            position_us,
            playing,
            start_us: anchor_us,
            next,
            valid_until_us,
            play_order: draft.play_order,
            round: draft.round,
            shuffled: draft.shuffled,
            loop_mode: draft.loop_mode,
        };
        plan.seq = match held {
            Some((held_term, held))
                if *held_term == term =>
            {
                let unchanged = GroupPlanDto {
                    seq: held.seq,
                    ..plan.clone()
                } == *held;
                if unchanged {
                    held.seq
                } else {
                    held.seq + 1
                }
            }
            _ => 1,
        };
        self.heard_ms = Some(now_ms);
        self.plan = Some((term, plan.clone()));
        Some((term, plan))
    }
}

/// 手上那份计划里，条目 `entry_id` 的时间线(锚点、那一刻的位置、在不在放):当前这一条，
/// 或者预告过的下一首(锚在预告的那一刻、从头放)。都不是就没有可接的。
fn timeline_of(
    plan: &GroupPlanDto,
    entry_id: i64,
) -> Option<(u64, u64, bool)> {
    if plan.entry_id == entry_id {
        return Some((
            plan.anchor_us,
            plan.position_us,
            plan.playing,
        ));
    }
    plan.next
        .as_ref()
        .filter(|next| next.entry_id == entry_id)
        .map(|next| (next.at_us, 0, true))
}

/// 实测(服务端时刻 `at_us` 媒体在 `position_us`)离「锚点 `anchor_us` 时在 `anchored_us`」
/// 那条线差多少微秒。
fn drift(
    anchor_us: u64,
    anchored_us: u64,
    at_us: u64,
    position_us: u64,
) -> u64 {
    let expected = anchored_us as i64
        + (at_us as i64 - anchor_us as i64);
    (expected - position_us as i64).unsigned_abs()
}

/// 计划在 `now_us` 这一刻的那一条：预告的下一首到点了就是它。
fn effective(
    plan: &GroupPlanDto,
    now_us: u64,
) -> Effective {
    let current = Effective {
        clock_epoch: plan.clock_epoch,
        queue_id: plan.queue_id,
        revision: plan.revision,
        entry_id: plan.entry_id,
        track: plan.track.clone(),
        anchor_us: plan.anchor_us,
        position_us: plan.position_us,
        playing: plan.playing,
        start_us: plan.start_us,
    };
    match &plan.next {
        Some(next)
            if plan.playing && now_us >= next.at_us =>
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
    }
}

#[cfg(test)]
mod tests;
