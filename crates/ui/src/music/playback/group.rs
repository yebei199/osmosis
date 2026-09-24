//! 本机作为播放组成员(#137 ⑤):跟随端照共同计划对准本机播放，主端把本机播放写成共同计划。
//!
//! 规则在 `app_core::Group`(收哪份计划、此刻该怎么出声、主端怎么写),这里只做接线：
//!
//! - **跟随端**:把计划换算到本机单调时钟上交给音频层的跟随器(`audio::sync::Target::Follow`)。
//!   计划要的那一首不在手上就按条目号取、起播;取不到就报故障 —— 不自己从头放、不换下一首。
//!   追赶期间不出声，对齐了才出声(跟随器自己管)。
//! - **主端**:本机照常自由地放(暂停、跳转、切歌都走原来那条路),定时把「服务端时刻 T
//!   媒体在 P」交给规则层写成计划、发出去;缓冲时写成暂停，恢复时重新定锚。
//!   一起开始的那一次(被叫「开始」)自己也静音等到计划定的那一刻，然后放开。
//!
//! 对准一拍 200ms:计划到了立刻对一次(`Shell.group-changed`),其余靠这一拍兜底。

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use app_core::{
    Cue, Draft, Effective, GroupRole, Intercepts, LoopMode,
    LoopModeDto, PlaybackState, Verdict,
};
use audio::sync::{Anchor, Target};

use crate::Shell;
use crate::music::*;

/// 多久对准一次。
const ALIGN_EVERY: Duration = Duration::from_millis(200);

/// 主端一起开始之后，再过多久放开时间线、改回自由播放。起播那一刻之后留一点余量：
/// 呈现时刻与这一拍的读钟不在同一瞬间。
const RELEASE_AFTER_START_NS: i64 = 100_000_000;

/// 共同计划里最多带多少条播放次序。
///
/// 一条信令超过 `MAX_SIGNAL_BYTES`(64KiB)整条连接就断;条目号按七八个字节算，三千条
/// 二十几 KiB,留足了余量。更长的队列交接时新主端按自己的次序往下放(ADR 0030 记着)。
const PLAY_ORDER_MAX: usize = 3_000;

/// 本机对准到了哪一步。
#[derive(Default)]
struct State {
    /// 本机播放器此刻跟着时间线(跟随端),而不是自由地放。
    following: bool,
    /// 为哪一条起的播:(队列, 版本, 条目)。同一条不重起。
    entry: Option<(i64, i64, i64)>,
    /// 正在取哪一版队列副本。
    fetching: Option<(i64, i64)>,
    /// 最近一次照着改过播放次序的那一份。
    order: Option<Vec<i64>>,
    /// 主端一起开始时静音等到哪一刻(本机单调时钟纳秒)。
    holding_until: Option<i64>,
    /// 报给遥控器的故障。
    fault: Option<String>,
    /// 「主端失联」那句话说过了。
    told_silent: bool,
    /// 主端实测的时间线截距，取中位数再写进计划。
    intercepts: Intercepts,
    /// 上一拍看到的欠载次数。多了就是这一拍之间断过粮(哪怕很短,这一拍没撞上):旧的截距作废。
    starves: u64,
}

/// 本机作为组成员的那份账。
#[derive(Clone, Default)]
pub(in crate::music) struct Alignment {
    inner: Rc<RefCell<State>>,
}

impl Alignment {
    /// 报给遥控器的故障(取不到计划要的那一首、跳不到位置)。
    pub(in crate::music) fn fault(&self) -> Option<String> {
        self.inner.borrow().fault.clone()
    }
}

/// 组或计划一变就对准一次，另起一拍兜底。
pub(in crate::music) fn bind_group(ui: &MainWindow, deck: &Deck) {
    let aligning = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Shell>().on_group_changed(move || {
        let Some(ui) = weak.upgrade() else { return };
        align(&ui, &aligning);
    });

    let deck = deck.clone();
    let weak = ui.as_weak();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, ALIGN_EVERY, move || {
        let Some(ui) = weak.upgrade() else { return };
        align(&ui, &deck);
    });
    // ponytail: 与自动续播那只定时器一样与进程同寿。
    Box::leak(Box::new(timer));
}

/// 按本机此刻在组里的身份对准一次。
pub(in crate::music) fn align(ui: &MainWindow, deck: &Deck) {
    let Ok(player) = deck.player.as_ref() else {
        return;
    };
    match deck.remote.group_role() {
        GroupRole::Solo => release(deck, player),
        GroupRole::Master => {
            free_after_start(deck, player);
            mirror(deck, player);
        }
        GroupRole::Follower => follow(ui, deck, player),
    }
}

/// 不在组里了：放开时间线，照本机自己的放。
fn release(deck: &Deck, player: &audio::Player) {
    let mut state = deck.alignment.inner.borrow_mut();
    if state.following || state.holding_until.is_some() {
        player.follow(Target::Free);
    }
    *state = State::default();
}

// ── 主端 ──

/// 被叫「开始」、而本机是主端(整组换人时的新主端):写下一起开始的那一份，自己也照它
/// 静音等到那一刻。要在起播之前调:时间线先交给跟随器，源一接上就按它等。
pub(in crate::music) fn begin_as_master(
    deck: &Deck,
    position_ms: u64,
    playing: bool,
) {
    if deck.remote.group_role() != GroupRole::Master {
        return;
    }
    let Some(draft) = draft(deck) else {
        return;
    };
    let position_us = position_ms * 1_000;
    let cue = if playing {
        Cue::Start { position_us }
    } else {
        Cue::Paused { position_us }
    };
    deck.remote.publish_group(draft, cue);
    let Some(plan) = deck.remote.group_plan().filter(|plan| plan.playing) else {
        return;
    };
    let (Some(at_ns), Some(start_ns)) = (
        deck.remote.to_local_ns(plan.clock_epoch, plan.anchor_us),
        deck.remote.to_local_ns(plan.clock_epoch, plan.start_us),
    ) else {
        return;
    };
    if let Ok(player) = deck.player.as_ref() {
        player.follow(Target::Follow {
            anchor: Anchor {
                at_ns,
                media_ns: (plan.position_us * 1_000) as i64,
            },
            playing: true,
            start_ns,
        });
        deck.alignment.inner.borrow_mut().holding_until = Some(start_ns);
    }
}

/// 一起开始的那一刻过去了就放开时间线;刚从跟随端交接成主端的，同样改回自由播放 ——
/// 它本来就对准在同一条时间线上，放开之后接着往下走。
fn free_after_start(deck: &Deck, player: &audio::Player) {
    let mut state = deck.alignment.inner.borrow_mut();
    if state.following {
        player.follow(Target::Free);
        state.following = false;
    }
    if state
        .holding_until
        .is_some_and(|until| audio::clock::monotonic_ns() > until + RELEASE_AFTER_START_NS)
    {
        player.follow(Target::Free);
        state.holding_until = None;
    }
}

/// 主端：把本机实际播放写成计划。
fn mirror(deck: &Deck, player: &audio::Player) {
    let Some(draft) = draft(deck) else {
        return;
    };
    let cue = if deck.alignment.inner.borrow().holding_until.is_some() {
        Cue::Keep
    } else {
        cue(deck, player)
    };
    deck.remote.publish_group(draft, cue);
}

/// 本机播放的样子。手上这一批还没同步到服务端(没有队列标识)就写不出计划：
/// 跟随端只能按服务端的标识取执行副本。
fn draft(deck: &Deck) -> Option<Draft> {
    let clock_epoch = deck.remote.clock_epoch()?;
    let queue = deck.queue.borrow();
    let track = queue.current()?.clone();
    let (queue_id, _, applied) = deck.execution.identity();
    let entry_id = deck.execution.entry_at(queue.index())?;
    let next = queue.peek_next_index().and_then(|index| {
        Some((deck.execution.entry_at(index)?, queue.tracks().get(index)?.clone()))
    });
    let play_order = (queue.order().len() <= PLAY_ORDER_MAX).then(|| {
        queue
            .order()
            .iter()
            .filter_map(|index| deck.execution.entry_at(*index))
            .collect()
    });
    Some(Draft {
        clock_epoch,
        queue_id: queue_id?,
        revision: applied?,
        entry_id,
        track,
        next,
        play_order,
        round: queue.round(),
        shuffled: queue.is_shuffled(),
        loop_mode: match queue.loop_mode() {
            LoopMode::Off => LoopModeDto::Off,
            LoopMode::All => LoopModeDto::All,
            LoopMode::One => LoopModeDto::One,
        },
    })
}

/// 本机此刻在干什么，写成哪一种时间线。
fn cue(deck: &Deck, player: &audio::Player) -> Cue {
    let state = deck.playback.borrow().state().clone();
    let position_us = player.position().as_micros() as u64;
    let mut alignment = deck.alignment.inner.borrow_mut();
    match state {
        // 换歌还在取流：先不动计划，跟随端照上一份(含预告的下一首)接着走。
        PlaybackState::Loading(_) => {
            alignment.intercepts.clear();
            Cue::Keep
        }
        PlaybackState::Playing(_) if !player.is_paused() => {
            let report = player.sync_report();
            // 缓冲(拉不到数据)时组时间线发布暂停，恢复时重新定锚(产品规则)。
            if !report.sounding {
                alignment.intercepts.clear();
                alignment.starves = report.starves;
                return Cue::Paused { position_us };
            }
            // 两拍之间断过粮:主端的声音真的慢了那一截，旧截距作废，按现在的重新定锚。
            if report.starves != alignment.starves {
                alignment.starves = report.starves;
                alignment.intercepts.clear();
            }
            let Some((present_ns, media)) = player.pairing() else {
                return Cue::Keep;
            };
            match deck.remote.to_server_us(present_ns) {
                Some(at_us) => Cue::Playing {
                    at_us,
                    position_us: alignment
                        .intercepts
                        .push(at_us, media.as_micros() as u64),
                },
                None => Cue::Keep,
            }
        }
        _ => {
            alignment.intercepts.clear();
            Cue::Paused { position_us }
        }
    }
}

// ── 跟随端 ──

fn follow(ui: &MainWindow, deck: &Deck, player: &audio::Player) {
    match deck.remote.group_verdict() {
        // 在成员里但还没被叫开始：手上原来在放的不动。
        Verdict::Solo => release(deck, player),
        Verdict::Waiting => hold(deck, player),
        Verdict::Expired => {
            hold(deck, player);
            let mut state = deck.alignment.inner.borrow_mut();
            if !state.told_silent {
                state.told_silent = true;
                drop(state);
                log::warn!("主端失联,已确认的计划放到头了:停下");
                crate::notice::show(
                    ui,
                    crate::sync::remote::describe_master_lost(),
                );
            }
        }
        Verdict::Follow(now) => track(ui, deck, player, &now),
    }
}

/// 不出声地等：停在当前位置，不消耗媒体。
fn hold(deck: &Deck, player: &audio::Player) {
    let now = audio::clock::monotonic_ns();
    player.resume();
    player.follow(Target::Follow {
        anchor: Anchor {
            at_ns: now,
            media_ns: player.position().as_nanos() as i64,
        },
        playing: false,
        start_ns: now,
    });
    deck.alignment.inner.borrow_mut().following = true;
}

/// 照这一条放：媒体对上，再把时间线交给跟随器。
fn track(ui: &MainWindow, deck: &Deck, player: &audio::Player, now: &Effective) {
    let (queue_id, _, applied) = deck.execution.identity();
    if (queue_id, applied) != (Some(now.queue_id), Some(now.revision)) {
        fetch_copy(ui, deck, now.queue_id, now.revision);
        hold(deck, player);
        return;
    }
    let want = (now.queue_id, now.revision, now.entry_id);
    if deck.alignment.inner.borrow().entry != Some(want) {
        let Some(index) = deck.execution.index_of(now.entry_id) else {
            deck.alignment.inner.borrow_mut().fault = Some(
                crate::sync::remote::describe_missing_entry(now.revision, now.entry_id),
            );
            hold(deck, player);
            return;
        };
        {
            let mut state = deck.alignment.inner.borrow_mut();
            state.entry = Some(want);
            state.fault = None;
        }
        start_entry(ui, deck, index, now);
    }
    if let PlaybackState::Failed(why) = deck.playback.borrow().state().clone() {
        deck.alignment.inner.borrow_mut().fault =
            Some(crate::sync::remote::describe_media_fault(&why));
        hold(deck, player);
        return;
    }
    apply_order(deck);
    let (Some(at_ns), Some(start_ns)) = (
        deck.remote.to_local_ns(now.clock_epoch, now.anchor_us),
        deck.remote.to_local_ns(now.clock_epoch, now.start_us),
    ) else {
        // 校时还没结论、或者计划是服务端上一次启动时写的：换算不了就不出声。
        hold(deck, player);
        return;
    };
    player.resume();
    player.follow(Target::Follow {
        anchor: Anchor {
            at_ns,
            media_ns: (now.position_us * 1_000) as i64,
        },
        playing: now.playing,
        start_ns,
    });
    deck.alignment.inner.borrow_mut().following = true;
}

/// 起播计划要的那一条，从它此刻该在的位置起。手上已经在放(或在取)这一条就不重起。
fn start_entry(ui: &MainWindow, deck: &Deck, index: usize, now: &Effective) {
    let current = deck.execution.entry_at(deck.queue.borrow().index());
    let busy = matches!(
        deck.playback.borrow().state(),
        PlaybackState::Playing(_) | PlaybackState::Loading(_)
    );
    if current == Some(now.entry_id) && busy {
        return;
    }
    let at_us = deck.remote.server_now_us().map_or(now.position_us, |now_us| {
        if now.playing && now_us >= now.start_us {
            now.position_us + (now_us - now.anchor_us.min(now_us))
        } else {
            now.position_us
        }
    });
    let _ = deck.queue.borrow_mut().jump_to(index);
    deck.start_at.set(Some(Start {
        at: Duration::from_micros(at_us),
        playing: true,
    }));
    play_current(ui, deck);
}

/// 取计划那一版的执行副本。正在放的那一条若也在新副本里，停在它上面，不打断。
fn fetch_copy(ui: &MainWindow, deck: &Deck, queue_id: i64, revision: i64) {
    {
        let mut state = deck.alignment.inner.borrow_mut();
        if state.fetching == Some((queue_id, revision)) {
            return;
        }
        state.fetching = Some((queue_id, revision));
    }
    let deck = deck.clone();
    let weak = ui.as_weak();
    let _ = slint::spawn_local(async move {
        let fetched = api::fetch_queue(queue_id, revision).await;
        deck.alignment.inner.borrow_mut().fetching = None;
        let entries = match fetched {
            Ok(entries) => entries,
            Err(error) => {
                deck.alignment.inner.borrow_mut().fault = Some(
                    crate::sync::remote::describe_copy_fault(&error.to_string()),
                );
                return;
            }
        };
        let playing = deck.execution.entry_at(deck.queue.borrow().index());
        let entry_ids: Vec<i64> = entries.iter().map(|entry| entry.entry_id).collect();
        let index = playing
            .and_then(|id| entry_ids.iter().position(|entry| *entry == id))
            .unwrap_or(0);
        let tracks = entries.into_iter().map(|entry| entry.track).collect();
        deck.queue.borrow_mut().replace(tracks, index);
        deck.execution.adopt(queue_id, revision, entry_ids);
        {
            let mut state = deck.alignment.inner.borrow_mut();
            state.entry = None;
            state.order = None;
        }
        if let Some(ui) = weak.upgrade() {
            align(&ui, &deck);
        }
    });
}

/// 照计划里的播放次序与循环模式改本机队列 —— 交接成主端时照着接着放。
fn apply_order(deck: &Deck) {
    let Some(plan) = deck.remote.group_plan() else {
        return;
    };
    deck.queue.borrow_mut().set_loop_mode(match plan.loop_mode {
        LoopModeDto::Off => LoopMode::Off,
        LoopModeDto::All => LoopMode::All,
        LoopModeDto::One => LoopMode::One,
    });
    let Some(order) = plan.play_order else {
        return;
    };
    if deck.alignment.inner.borrow().order.as_ref() == Some(&order) {
        return;
    }
    let Some(indices) = deck.execution.indices_of(&order) else {
        return;
    };
    if deck.queue.borrow_mut().restore_order(indices, plan.shuffled) {
        deck.alignment.inner.borrow_mut().order = Some(order);
    }
}

/// 跟随端只放计划说的那一首：本机自己的自动续播、断流切歌、起播上报都停。
pub(in crate::music) fn follows_the_group(deck: &Deck) -> bool {
    deck.remote.group_role() == GroupRole::Follower
        && deck.alignment.inner.borrow().following
}
