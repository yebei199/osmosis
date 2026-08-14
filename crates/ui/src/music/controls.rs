//! 传输控件的绑定:播放键、上一首下一首、洗牌循环、音量与跳转。

use super::*;
use crate::Player;
use slint::ComponentHandle as _;

/// 点一首歌:这一批成为队列、从这首开始放(见 `CONTEXT.md`「队列」)。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn bind_play(ui: &MainWindow, deck: &Deck) {
    let deck = deck.clone();
    let weak = ui.as_weak();

    ui.global::<Player>().on_play(move |id| {
        let Some(ui) = weak.upgrade() else { return };

        // 这一首已经在加载了:这一下是多余的,直接丢掉。不挡的话,连点五下
        // 就是五条在途下载,每条回来都往播放器里塞一次源,声音从头响五遍。
        let redundant = is_redundant_tap(
            deck.playback.borrow().state(),
            &id,
        );
        if redundant {
            return;
        }

        // 点歌是播放动作:正在收听的话,先退出(CONTEXT.md「听众」)。
        if deck.sync.is_listening() {
            deck.sync.leave();
        }

        let id = id.to_string();
        let batch = deck.tracks.borrow().clone();
        let Some(index) =
            batch.iter().position(|track| track.id == id)
        else {
            return;
        };

        // replace 把随机清掉(新批还没洗过),开着的话补洗一次把它立回去。
        deck.queue.borrow_mut().replace(batch, index);
        if ui.global::<Player>().get_shuffle_on() {
            deck.queue.borrow_mut().shuffle(shuffle_seed());
        }
        play_current(&ui, &deck);
    });
}

/// 控制条:播放/暂停、上一首/下一首、随机开关。
///
/// 收听中的任何一键都先退出收听;⏯ 到此为止(退出即静音,再按才操作自己的
/// 队列),切歌键退出后紧接着作用于本机队列 —— 点了"下一首"的人想听的是
/// 自己的下一首,不是单纯安静下来。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn bind_controls(ui: &MainWindow, deck: &Deck) {
    let toggle = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_toggle_play(move || {
        let Some(ui) = weak.upgrade() else { return };
        toggle_play(&ui, &toggle);
        // 暂停图标不该慢一拍 —— 轮询要 1 秒之后才轮到。
        crate::media::push(
            &ui,
            &toggle.playback,
            &toggle.media,
        );
    });

    let next = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_next_track(move || {
        let Some(ui) = weak.upgrade() else { return };
        if next.sync.is_listening() {
            next.sync.leave();
        }
        advance(&ui, &next);
    });

    let previous = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_prev_track(move || {
        let Some(ui) = weak.upgrade() else { return };
        if previous.sync.is_listening() {
            previous.sync.leave();
        }
        if previous.queue.borrow_mut().previous().is_some()
        {
            play_current(&ui, &previous);
        }
    });

    let shuffle = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_shuffle_toggled(move || {
        let Some(ui) = weak.upgrade() else { return };
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
        let next = match looper.queue.borrow().loop_mode() {
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
pub(super) fn apply_loop(
    ui: &MainWindow,
    deck: &Deck,
    mode: app_core::LoopMode,
) {
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
pub(super) fn bind_volume(ui: &MainWindow, deck: &Deck) {
    let saved = api::settings::load().volume;
    ui.global::<Player>().set_volume(saved);
    if let Ok(player) = deck.player.as_ref() {
        player.set_volume(saved);
    }

    let deck = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_volume_changed(
        move |volume| {
            let volume = audio::clamped_volume(volume);
            if let Some(ui) = weak.upgrade() {
                ui.global::<Player>().set_volume(volume);
            }
            if let Ok(player) = deck.player.as_ref() {
                player.set_volume(volume);
            }

            // 每动一下就存:调音量是个连续动作,而"什么时候算调完了"没有信号。
            // 写的是本地一个几十字节的文件,存不下也只是下次回到默认值。
            //
            // **先读再改**:整份重造的话,这个文件里别的设置(明暗)会被这次
            // 调音量顺手冲回默认值。
            api::settings::save(&api::settings::Settings {
                volume,
                ..api::settings::load()
            });
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
pub(super) fn bind_seek(ui: &MainWindow, deck: &Deck) {
    let deck = deck.clone();
    let weak = ui.as_weak();

    ui.global::<Player>().on_seek(move |at| {
        let Some(ui) = weak.upgrade() else { return };
        let state = deck.playback.borrow().state().clone();
        let (PlaybackState::Playing(track)
        | PlaybackState::Loading(track)) = state
        else {
            return;
        };
        let Some(target) = crate::progress::seek_target(
            at,
            track.duration_ms,
        ) else {
            return;
        };

        // 立刻挂上,不等轮询:那要慢一秒,而一秒的沉默正好是"点了没反应"
        ui.global::<Player>().set_buffering(true);

        if let Ok(player) = deck.player.as_ref()
            && let Err(err) = player.seek(target)
        {
            ui.global::<Player>().set_buffering(false);
            crate::notice::show(
                &ui,
                format!("这首跳不了: {err}"),
            );
        }
    });
}

/// 把跳转的下场推给界面。
///
/// 两个出口而不是一个:还在取字节 -> 挂着「缓冲中」;试过了不行 -> 摘掉缓冲
/// 并说一句为什么。少了后一条,跳不了的歌会永远停在「缓冲中」上,
/// 而那比一开始就说"跳不了"更难查。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn push_seek_state(
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
