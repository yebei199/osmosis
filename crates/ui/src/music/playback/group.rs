//! 本机作为播放组成员(#142):照服务端的全局状态对准本机播放。
//!
//! 规则在 `app_core::GlobalGroup`(本机是独奏 / 只当遥控器 / 出声设备、此刻该放哪一条),
//! 这里只做接线:
//!
//! - **出声设备**:把状态换算到本机单调时钟上交给音频层的跟随器(`audio::sync::Target::Follow`)。
//!   状态要的那一首不在手上就按条目号取、起播;取不到就报故障 —— 不自己从头放、不换下一首。
//!   追赶期间不出声,对齐了才出声(跟随器自己管)。与服务端断开时停在原地(掉线规则)。
//! - **只当遥控器**:本机播放器停着。
//! - **退出组**(或组散了):本机停下 —— 「出声设备主动退出组,它自己停」。
//!
//! 不再有主端:谁都不写计划,状态只由服务端写。对准一拍 200ms:状态到了立刻对一次
//! (`Shell.group-changed`),其余靠这一拍兜底。

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use app_core::{Effective, PlaybackState, Sound};
use audio::sync::{Anchor, Target};

use crate::Shell;
use crate::music::*;

/// 多久对准一次。
const ALIGN_EVERY: Duration = Duration::from_millis(200);

/// 本机对准到了哪一步。
#[derive(Default)]
struct State {
    /// 上一拍本机在不在组里。从组里出来的那一拍要把本机停下。
    member: bool,
    /// 本机播放器此刻跟着时间线,而不是自由地放。
    following: bool,
    /// 只当遥控器时已经把本机停过了。
    silenced: bool,
    /// 为哪一条起的播:(队列, 版本, 条目, 起播时刻)。同一条不重起;单曲循环每一遍的
    /// 起播时刻不同,所以带上它。
    entry: Option<(i64, i64, i64, u64)>,
    /// 正在取哪一版队列副本。
    fetching: Option<(i64, i64)>,
    /// 报给组里其他设备的故障。
    fault: Option<String>,
}

/// 本机作为组成员的那份账。
#[derive(Clone, Default)]
pub(in crate::music) struct Alignment {
    inner: Rc<RefCell<State>>,
}

impl Alignment {
    /// 报给组里其他设备的故障(取不到状态要的那一首、跳不到位置)。
    pub(in crate::music) fn fault(&self) -> Option<String> {
        self.inner.borrow().fault.clone()
    }
}

/// 组或状态一变就对准一次,另起一拍兜底。
pub(in crate::music) fn bind_group(
    ui: &MainWindow,
    deck: &Deck,
) {
    let aligning = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Shell>().on_group_changed(move || {
        let Some(ui) = weak.upgrade() else { return };
        align(&ui, &aligning);
    });

    let deck = deck.clone();
    let weak = ui.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        ALIGN_EVERY,
        move || {
            let Some(ui) = weak.upgrade() else { return };
            align(&ui, &deck);
        },
    );
    // ponytail: 与自动续播那只定时器一样与进程同寿。
    Box::leak(Box::new(timer));
}

/// 按本机此刻在组里的身份对准一次。
pub(in crate::music) fn align(
    ui: &MainWindow,
    deck: &Deck,
) {
    let Ok(player) = deck.player.as_ref() else {
        return;
    };
    let sound = deck.group.sound();
    let was_member = deck.alignment.inner.borrow().member;
    deck.alignment.inner.borrow_mut().member =
        sound != Sound::Solo;
    match sound {
        Sound::Solo => {
            release(deck, player);
            // 从组里出来:本机停下,不接着放组里那一首(掉线规则)。
            if was_member {
                rest_local(ui, deck);
            }
        }
        Sound::Silent => silence(ui, deck, player),
        Sound::Hold => hold(deck, player),
        Sound::Follow(now) => track(ui, deck, player, &now),
    }
}

/// 不在组里了:放开时间线,照本机自己的放。
fn release(deck: &Deck, player: &audio::Player) {
    let mut state = deck.alignment.inner.borrow_mut();
    if state.following {
        player.follow(Target::Free);
    }
    *state = State::default();
}

/// 只当遥控器:本机停着。停一次就够,之后每拍什么都不做。
fn silence(
    ui: &MainWindow,
    deck: &Deck,
    player: &audio::Player,
) {
    let mut state = deck.alignment.inner.borrow_mut();
    if state.silenced {
        return;
    }
    if state.following {
        player.follow(Target::Free);
    }
    *state = State {
        member: true,
        silenced: true,
        ..State::default()
    };
    drop(state);
    rest_local(ui, deck);
}

/// 不出声地等:停在当前位置,不消耗媒体。
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
    let mut state = deck.alignment.inner.borrow_mut();
    state.following = true;
    state.silenced = false;
}

/// 照这一条放:媒体对上,再把时间线交给跟随器。
fn track(
    ui: &MainWindow,
    deck: &Deck,
    player: &audio::Player,
    now: &Effective,
) {
    deck.alignment.inner.borrow_mut().silenced = false;
    let (queue_id, _, applied) = deck.execution.identity();
    if (queue_id, applied)
        != (Some(now.queue_id), Some(now.revision))
    {
        fetch_copy(ui, deck, now.queue_id, now.revision);
        hold(deck, player);
        return;
    }
    let want = (
        now.queue_id,
        now.revision,
        now.entry_id,
        now.start_us,
    );
    if deck.alignment.inner.borrow().entry != Some(want) {
        let Some(index) =
            deck.execution.index_of(now.entry_id)
        else {
            deck.alignment.inner.borrow_mut().fault = Some(
                crate::sync::group::describe_missing_entry(
                    now.revision,
                    now.entry_id,
                ),
            );
            hold(deck, player);
            return;
        };
        {
            let mut state =
                deck.alignment.inner.borrow_mut();
            state.entry = Some(want);
            state.fault = None;
        }
        start_entry(ui, deck, index, now);
    }
    if let PlaybackState::Failed(why) =
        deck.playback.borrow().state().clone()
    {
        deck.alignment.inner.borrow_mut().fault = Some(
            crate::sync::group::describe_media_fault(&why),
        );
        hold(deck, player);
        return;
    }
    let (Some(at_ns), Some(start_ns)) = (
        deck.group
            .to_local_ns(now.clock_epoch, now.anchor_us),
        deck.group
            .to_local_ns(now.clock_epoch, now.start_us),
    ) else {
        // 校时还没结论、或者状态是服务端上一次启动时写的:换算不了就不出声。
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

/// 起播状态要的那一条,从它此刻该在的位置起。手上已经在放(或在取)这一条就不重起 ——
/// 除非它已经放完了(单曲循环的下一遍)。
fn start_entry(
    ui: &MainWindow,
    deck: &Deck,
    index: usize,
    now: &Effective,
) {
    let current = deck
        .execution
        .entry_at(deck.queue.borrow().index());
    let busy = matches!(
        deck.playback.borrow().state(),
        PlaybackState::Playing(_)
            | PlaybackState::Loading(_)
    ) && deck
        .player
        .as_ref()
        .as_ref()
        .is_ok_and(|player| !player.empty());
    if current == Some(now.entry_id) && busy {
        return;
    }
    let at_us = deck.group.server_now_us().map_or(
        now.position_us,
        |now_us| {
            if now.playing && now_us >= now.start_us {
                now.position_us
                    + (now_us - now.anchor_us.min(now_us))
            } else {
                now.position_us
            }
        },
    );
    let _ = deck.queue.borrow_mut().jump_to(index);
    deck.start_at.set(Some(Start {
        at: Duration::from_micros(at_us),
        playing: true,
    }));
    play_current(ui, deck);
}

/// 取状态那一版的执行副本。正在放的那一条若也在新副本里,停在它上面,不打断。
fn fetch_copy(
    ui: &MainWindow,
    deck: &Deck,
    queue_id: i64,
    revision: i64,
) {
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
        let fetched =
            api::fetch_queue(queue_id, revision).await;
        deck.alignment.inner.borrow_mut().fetching = None;
        let entries = match fetched {
            Ok(entries) => entries,
            Err(error) => {
                deck.alignment.inner.borrow_mut().fault =
                    Some(crate::sync::group::describe_copy_fault(
                        &error.to_string(),
                    ));
                return;
            }
        };
        let playing = deck
            .execution
            .entry_at(deck.queue.borrow().index());
        let entry_ids: Vec<i64> = entries
            .iter()
            .map(|entry| entry.entry_id)
            .collect();
        let index = playing
            .and_then(|id| {
                entry_ids
                    .iter()
                    .position(|entry| *entry == id)
            })
            .unwrap_or(0);
        let tracks = entries
            .into_iter()
            .map(|entry| entry.track)
            .collect();
        deck.queue.borrow_mut().replace(tracks, index);
        deck.execution.adopt(queue_id, revision, entry_ids);
        deck.alignment.inner.borrow_mut().entry = None;
        if let Some(ui) = weak.upgrade() {
            align(&ui, &deck);
        }
    });
}

/// 本机在组里:放哪一首只听全局状态,本机自己的自动续播、断流切歌、起播上报都停
/// (起播由服务端记,#142)。
pub(in crate::music) fn follows_the_group(
    deck: &Deck,
) -> bool {
    deck.group.is_member()
}
