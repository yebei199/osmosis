//! 传输控件的绑定:播放键、上一首下一首、洗牌循环、音量与跳转。
//!
//! 这里只做 **translate**:把用户按下的那一下翻成一条 [`Intent`],交给
//! [`dispatch`]。谁能按、按给哪台设备、翻成什么命令,一概不在这个文件里 ——
//! 从前那两道闸(被遥控时不生效 / 输出不在本机时改发命令)在下面六个回调里
//! 各写了一遍,写法还不一致,于是总有一个被漏掉,而漏掉的那个在界面上表现为
//! 「别的键都遥控,唯独这个还在本机上放」(#108)。

use super::*;
use crate::Player;
use crate::music::*;

/// 点一首歌:这一批成为队列、从这首开始放(见 `CONTEXT.md`「队列」)。
///
/// 批次取**用户点中的那个列表**,不是被控端回报的队列 —— 遥控时他想听的
/// 也是眼前这一批的后面那些歌。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn bind_play(
    ui: &MainWindow,
    deck: &Deck,
) {
    let deck = deck.clone();
    let weak = ui.as_weak();

    ui.global::<Player>().on_play(move |id| {
        let Some(ui) = weak.upgrade() else { return };

        let id = id.to_string();
        let tracks = deck.tracks.borrow().clone();
        let Some(index) =
            tracks.iter().position(|track| track.id == id)
        else {
            // 点中的那首不在这一批里。翻不出意图,没什么可派发的。
            return;
        };

        dispatch(
            &ui,
            &deck,
            Intent::Play { tracks, index },
        );
    });
}

/// 控制条:播放/暂停、上一首/下一首、随机开关。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn bind_controls(
    ui: &MainWindow,
    deck: &Deck,
) {
    let toggle = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_toggle_play(move || {
        let Some(ui) = weak.upgrade() else { return };
        dispatch(&ui, &toggle, Intent::TogglePlay);
        // 暂停图标不该慢一拍 —— 轮询要 1 秒之后才轮到。
        crate::media::push(
            &ui,
            &toggle.playback,
            &toggle.media,
        );
    });

    let focus = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_focus_changed(move |held| {
        let Some(ui) = weak.upgrade() else { return };
        focus_changed(&ui, &focus, held);
    });

    let next = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_next_track(move || {
        let Some(ui) = weak.upgrade() else { return };
        dispatch(&ui, &next, Intent::Next);
    });

    let previous = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_prev_track(move || {
        let Some(ui) = weak.upgrade() else { return };
        dispatch(&ui, &previous, Intent::Prev);
    });

    let shuffle = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_shuffle_toggled(move || {
        let Some(ui) = weak.upgrade() else { return };
        // 组里的随机是全局状态的一部分(#142):发给服务端,开关跟着状态走。
        if shuffle.group.is_member() {
            let on = !group_shuffled(&shuffle);
            shuffle.group.transport(
                &ui,
                app_core::TransportOpDto::Shuffle { on },
            );
            return;
        }
        let on = {
            let mut queue = shuffle.queue.borrow_mut();
            if queue.is_shuffled() {
                queue.unshuffle();
            } else {
                queue.shuffle(shuffle_seed());
            }
            queue.is_shuffled()
        };
        // 界面上那个开关是这一位的投影,拨完由这里写回去 —— 开关自己不置位。
        ui.global::<Player>().set_shuffle_on(on);
        // 系统控件上的随机也该立刻跟着翻,轮询要 1 秒之后才轮到。
        crate::media::push(
            &ui,
            &shuffle.playback,
            &shuffle.media,
        );
    });

    let looper = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_loop_cycled(move || {
        let Some(ui) = weak.upgrade() else { return };
        use app_core::LoopMode;
        // 关→列表→单曲→关:单键三态,读的是队列里的真相,不是界面属性。
        let current = if looper.group.is_member() {
            group_loop(&looper)
        } else {
            looper.queue.borrow().loop_mode()
        };
        let next = match current {
            LoopMode::Off => LoopMode::All,
            LoopMode::All => LoopMode::One,
            LoopMode::One => LoopMode::Off,
        };
        apply_loop(&ui, &looper, next);
    });

    let setter = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_loop_mode_set(move |mode| {
        let Some(ui) = weak.upgrade() else { return };
        apply_loop(
            &ui,
            &setter,
            crate::media::loop_from_index(mode),
        );
    });
}

/// 循环模式落到队列,把投影写回界面,并立刻推给系统媒体控件 ——
/// 轮询要 1 秒之后才轮到,锁屏上的键不该慢一拍。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn apply_loop(
    ui: &MainWindow,
    deck: &Deck,
    mode: app_core::LoopMode,
) {
    // 组里的循环是全局状态的一部分(#142)。
    if deck.group.is_member() {
        deck.group.transport(
            ui,
            app_core::TransportOpDto::Loop {
                mode: match mode {
                    app_core::LoopMode::Off => {
                        app_core::LoopModeDto::Off
                    }
                    app_core::LoopMode::All => {
                        app_core::LoopModeDto::All
                    }
                    app_core::LoopMode::One => {
                        app_core::LoopModeDto::One
                    }
                },
            },
        );
        return;
    }
    deck.queue.borrow_mut().set_loop_mode(mode);
    ui.global::<Player>()
        .set_loop_mode(crate::media::loop_index(mode));
    crate::media::push(ui, &deck.playback, &deck.media);
}

/// 接上音量:开局从本地设置恢复,拖动时既改播放器也存回去。
///
/// 音量跟着设备走,不跟着账号 —— 笔记本外放与一副耳机不该共用一个数值
/// (见 api::settings)。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn bind_volume(
    ui: &MainWindow,
    deck: &Deck,
) {
    let saved = api::settings::load().volume;
    ui.global::<Player>().set_volume(saved);
    if let Ok(player) = deck.player.as_ref() {
        player.set_volume(saved);
    }

    let deck = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_volume_changed(
        move |volume| {
            let Some(ui) = weak.upgrade() else { return };
            // 滑块先跟手。这一下最终落到哪台设备上由 `dispatch` 定,但拖着
            // 不动的滑块看起来就是失灵了。
            let volume = audio::clamped_volume(volume);
            ui.global::<Player>().set_volume(volume);

            // 音量每台各自调(#142),在组里也落在本机。
            dispatch(
                &ui,
                &deck,
                Intent::Volume { level: volume },
            );
        },
    );
}

/// 接上进度条的拖动。
///
/// 跳转有**两种下场,两条报告路径**(见 `audio::ChannelSource::try_seek`):
///
/// - 当场就知道跳不动(格式不支持、这条流只进不退):`seek` 直接返回 `Err`,
///   这里当场说。那一刻 rodio 的位置计数器根本没动过,进度条与声音仍然一致。
/// - 真在取字节:`seek` 乐观返回 `Ok`,这里挂上「缓冲中」,结论由每秒那趟
///   轮询从 `audio::SeekState` 上取(`push_seek_state`)。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn bind_seek(
    ui: &MainWindow,
    deck: &Deck,
) {
    let deck = deck.clone();
    let weak = ui.as_weak();

    ui.global::<Player>().on_seek(move |at| {
        let Some(ui) = weak.upgrade() else { return };
        // 只带比例:目标毫秒要按**这一下落在哪一端**报的曲长算,而那是
        // `dispatch` 选完目标才知道的事。
        dispatch(&ui, &deck, Intent::Seek { ratio: at });
    });
}

/// 把跳转的下场推给界面。
///
/// 两个出口而不是一个:还在取字节 -> 挂着「缓冲中」;试过了不行 -> 摘掉缓冲
/// 并说一句为什么。少了后一条,跳不了的歌会永远停在「缓冲中」上,
/// 而那比一开始就说"跳不了"更难查。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn push_seek_state(
    ui: &MainWindow,
    deck: &Deck,
) {
    let borrowed = deck.seeking.borrow();
    let Some(state) = borrowed.as_ref() else {
        return;
    };

    if let Some(why) = state.take_failure() {
        ui.global::<Player>().set_buffering(false);
        crate::notice::show(
            ui,
            format!("这首跳不了: {why}"),
        );
        return;
    }

    ui.global::<Player>().set_buffering(state.is_seeking());
}

/// 组里此刻开没开随机。
#[cfg(not(target_arch = "wasm32"))]
fn group_shuffled(deck: &Deck) -> bool {
    deck.group
        .state()
        .and_then(|state| state.now)
        .is_some_and(|now| now.shuffled)
}

/// 组里此刻的循环模式。
#[cfg(not(target_arch = "wasm32"))]
fn group_loop(deck: &Deck) -> app_core::LoopMode {
    use app_core::{LoopMode, LoopModeDto};
    match deck
        .group
        .state()
        .and_then(|state| state.now)
        .map(|now| now.loop_mode)
    {
        Some(LoopModeDto::All) => LoopMode::All,
        Some(LoopModeDto::One) => LoopMode::One,
        _ => LoopMode::Off,
    }
}
