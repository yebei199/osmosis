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

use crate::music::*;
use crate::{Player, Shell};

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
    /// 与服务端断开时停过:连回来要按最新状态把这一首重新起一遍,不在停下的那个流上接着追。
    /// 断开几十秒后,输出流已经关了、媒体连接多半也被对端掐了,原地追赶会卡死在停下的
    /// 位置(#142 F-5)。
    stale: bool,
    /// 本机丢了音频焦点(来电、别的应用抢了):只停本机的声音,组照放;拿回来之后照状态
    /// 重新跟上(#142 AC-10)。
    focus_lost: bool,
    /// 报给组里其他设备的故障。
    fault: Option<String>,
}

/// 本机作为组成员的那份账。
#[derive(Clone, Default)]
pub(in crate::music) struct Alignment {
    inner: Rc<RefCell<State>>,
}

impl Alignment {
    /// 本机此刻被记成丢了音频焦点。
    #[cfg(test)]
    pub(in crate::music) fn focus_lost(&self) -> bool {
        self.inner.borrow().focus_lost
    }

    /// 下一次照状态放时要把这一首重新起一遍(断线重连、焦点回来)。
    #[cfg(test)]
    pub(in crate::music) fn restarts_next(&self) -> bool {
        self.inner.borrow().stale
    }

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
    let sound = deck.group.sound();
    let was_member = deck.alignment.inner.borrow().member;
    deck.alignment.inner.borrow_mut().member =
        sound != Sound::Solo;
    // 控制条的 ⏯ 照全局状态画:出声设备的播放器在组里一直「没按暂停」,暂停的是时间线
    // (#142 F-4)。只当遥控器的由 `Group::push_playback` 画。
    match &sound {
        Sound::Follow(now) => {
            ui.global::<Player>()
                .set_is_playing(now.playing);
        }
        Sound::Hold => {
            ui.global::<Player>().set_is_playing(false)
        }
        Sound::Solo | Sound::Silent => {}
    }
    let Ok(player) = deck.player.as_ref() else {
        return;
    };
    match sound {
        Sound::Solo => {
            release(deck, player);
            // 从组里出来:本机停下,不接着放组里那一首(掉线规则)。
            if was_member {
                rest_local(ui, deck);
            }
        }
        Sound::Silent => silence(ui, deck, player),
        // 丢了焦点:本机不出声,等拿回来再照状态重新起(和断线重连同一条路)。
        Sound::Follow(_)
            if deck.alignment.inner.borrow().focus_lost =>
        {
            deck.alignment.inner.borrow_mut().stale = true;
            hold(deck, player);
        }
        Sound::Hold => {
            if !deck.group.is_online() {
                deck.alignment.inner.borrow_mut().stale =
                    true;
            }
            hold(deck, player);
        }
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
    // 断开后连回来:这一首从状态此刻的位置重新起,见 `State::stale`。
    let reload = {
        let mut state = deck.alignment.inner.borrow_mut();
        state.silenced = false;
        if state.stale {
            state.entry = None;
        }
        state.stale
    };
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
            state.stale = false;
        }
        start_entry(ui, deck, index, now, reload);
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
    reload: bool,
) {
    let current = deck
        .execution
        .entry_at(deck.queue.borrow().index());
    // 手上在放的得真是这一首:换了一版队列之后,队列的游标可能恰好落在同一个条目号上,
    // 播放器里却还是上一版的那首歌(#142 F-3,从另一份搜索结果点歌后两台重放了上一首)。
    let busy = matches!(
        deck.playback.borrow().state(),
        PlaybackState::Playing(track)
            | PlaybackState::Loading(track)
            if track.id == now.track.id
    ) && deck
        .player
        .as_ref()
        .as_ref()
        .is_ok_and(|player| !player.empty());
    if !reload && current == Some(now.entry_id) && busy {
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

/// 系统说本机丢了 / 拿回了音频焦点(安卓的 `onAudioFocusChange`)。
///
/// 独奏时与从前一样:丢了当暂停、拿回当继续(只在会改变状态时才按那一下)。在组里**不发
/// 任何意图** —— 用户定的「只停这台自己」(#142 AC-10):丢了本机停下、组照放,拿回了照
/// 全局状态接着跟。只当遥控器的设备不出声,记下来也不影响什么。
pub(in crate::music) fn focus_changed(
    ui: &MainWindow,
    deck: &Deck,
    held: bool,
) {
    if deck.group.is_member() {
        {
            let mut state =
                deck.alignment.inner.borrow_mut();
            state.focus_lost = !held;
            // 丢了焦点这一刻就记下「回来要重起」,不等对准那一拍:永久丢失时安卓那边
            // 已经放掉了输出,拿回焦点只可能是重新申请当场批准(#142 N-4),原地追不回来。
            if !held {
                state.stale = true;
            }
        }
        align(ui, deck);
        return;
    }
    if held != ui.global::<Player>().get_is_playing() {
        ui.global::<Player>().invoke_toggle_play();
    }
}

/// 本机(出声设备)真正放完了全局状态此刻那一首:手上放的就是那一条的那首歌。
///
/// 播放器放空不够:换版取副本、条目不在、校时没结论时播放器也可能是空的,那不是放完了,
/// 报上去会把组推到下一首(#142 AC-9)。
pub(in crate::music) fn finished_the_entry(
    deck: &Deck,
) -> bool {
    let Some(now) = deck.group.now() else {
        return false;
    };
    let started =
        deck.alignment.inner.borrow().entry.is_some_and(
            |(_, _, entry, _)| entry == now.entry_id,
        );
    started
        && matches!(
            deck.playback.borrow().state(),
            PlaybackState::Playing(track) if track.id == now.track.id
        )
}

/// 本机在组里:放哪一首只听全局状态,本机自己的自动续播、断流切歌、起播上报都停
/// (起播由服务端记,#142)。
pub(in crate::music) fn follows_the_group(
    deck: &Deck,
) -> bool {
    deck.group.is_member()
}
