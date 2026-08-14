//! 系统媒体控件的接缝。
//!
//! 宿主系统各有各的说法 —— Linux 上是 session bus 的 `org.mpris.MediaPlayer2.*`,
//! 安卓上是 `MediaSession` 加一条前台通知 —— 但要的东西是同一样:此刻在放什么,
//! 以及外面按下的键该落到哪。这里定义那个共同的形状,真正说各自方言的代码住在
//! 平台入口 crate(`apps/desktop`、`apps/android`),理由见 `docs/adr/0020`。
//!
//! 界面层这一侧多做一步、后端少做一步:`Play` 与 `Toggle` 的区分、相对跳转换成
//! 绝对位置、绝对位置换成 Slint 要的比例,全在这里做完。后端因此不必记住任何
//! 状态 —— 两个后端各记一份「现在是不是在放」,迟早会有一份是错的。

use std::sync::Arc;

// 下面这些只有原生那半边用得到 —— wasm 上界面在、播放不在,那半边整个不存在
// (见下方「以下到文件末尾是原生那一半」)。
#[cfg(not(target_arch = "wasm32"))]
use core::cell::RefCell;

#[cfg(not(target_arch = "wasm32"))]
use app_core::Playback;
#[cfg(not(target_arch = "wasm32"))]
use slint::ComponentHandle;

#[cfg(not(target_arch = "wasm32"))]
use crate::MainWindow;
use crate::viz::CoverPixels;

mod rules;
mod seam;

pub use seam::{
    MediaCommand, MediaControls, MediaHooks, MediaStatus,
    NoControls, NowPlaying,
};

pub(crate) use rules::{loop_from_index, loop_index};

use crate::Player;
use rules::*;

/// 界面这一侧的媒体控件把手:后端,加上推给它的那些东西的最新一份。
///
/// 去重放在这里而不是各个后端里:推送搭的是 1Hz 的续播轮询,不去重的话一首
/// 四分钟的歌会往外发两百多次状态变更,而内容一个字都没变。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Bridge {
    controls: Box<dyn MediaControls>,
    last: RefCell<Option<NowPlaying>>,
    /// 当前这一首的封面。晚于歌名到达(要过一趟网络),所以单独存。
    art: RefCell<Option<Arc<CoverPixels>>>,
    /// 当前这一首的时长,毫秒。跳转要靠它把绝对位置换成比例,而那道换算发生在
    /// 后端线程送来的命令里 —— 跨线程,所以是 `Arc<AtomicI64>` 而不是 `RefCell`。
    ///
    /// 由外面造好再交进来:命令闭包得在后端存在之前就捏好(它要被交给后端),
    /// 而后端造出来之后才有这个 `Bridge`。
    duration_ms: Arc<core::sync::atomic::AtomicI64>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Bridge {
    pub(crate) fn new(
        controls: Box<dyn MediaControls>,
        duration_ms: Arc<core::sync::atomic::AtomicI64>,
    ) -> Self {
        Self {
            controls,
            last: RefCell::new(None),
            art: RefCell::new(None),
            duration_ms,
        }
    }

    /// 换歌了,封面重新开始等。
    pub(crate) fn clear_art(&self) {
        *self.art.borrow_mut() = None;
    }

    /// 封面到了。
    pub(crate) fn set_art(&self, art: Arc<CoverPixels>) {
        *self.art.borrow_mut() = Some(art);
    }

    pub(crate) fn art(&self) -> Option<Arc<CoverPixels>> {
        self.art.borrow().clone()
    }

    /// 推出去,除非跟上次一模一样。
    pub(crate) fn publish(&self, now: NowPlaying) {
        self.duration_ms.store(
            now.duration_ms,
            core::sync::atomic::Ordering::Relaxed,
        );

        let mut last = self.last.borrow_mut();
        if last.as_ref().is_some_and(|last| {
            last.fingerprint() == now.fingerprint()
        }) {
            return;
        }

        self.controls.publish(&now);
        *last = Some(now);
    }
}

/// 接上系统媒体控件:把两根线捏好交给平台入口,换回它那一端的实现。
///
/// 位置直接问播放器 —— 它是 `Send + Sync`(同播的事件本就在后台线程上用它),
/// 后端在自己的线程上拉不必绕回 UI 线程。命令反过来必须绕回来:Slint 的回调
/// 只能在事件循环上调。
///
/// 时长单独拿一个原子:跳转要靠它把绝对位置换成比例,而那道换算发生在后端
/// 送来的命令里,跨线程。它在这里造好,一份进闭包、一份进 [`Bridge`]。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn bind(
    ui: &MainWindow,
    player: &Arc<Result<audio::Player, audio::AudioError>>,
    media: impl FnOnce(MediaHooks) -> Box<dyn MediaControls>,
) -> Bridge {
    let duration =
        Arc::new(std::sync::atomic::AtomicI64::new(0));

    let hooks = MediaHooks {
        command: {
            let weak = ui.as_weak();
            let player = player.clone();
            let duration = duration.clone();
            Arc::new(move |command| {
                let player = player.clone();
                let duration = duration.clone();
                // 失败只有一种成因:事件循环已经没了。那时界面也不在了。
                weak.upgrade_in_event_loop(move |ui| {
                    dispatch(
                        &ui, &player, &duration, command,
                    );
                })
                .ok();
            })
        },
        position: {
            let player = player.clone();
            Arc::new(move || {
                player
                    .as_ref()
                    .as_ref()
                    .map(audio::Player::position)
                    .unwrap_or_default()
            })
        },
    };

    Bridge::new(media(hooks), duration)
}

/// 系统媒体控件按下的键落到界面上。
///
/// **只调 `.slint` 的回调,不碰任何状态。** `music::bind_controls` 里已经有一整套规矩
/// (收听同播时先退出、放空了就重放当前曲),在这里重写一遍就会立刻长歪。
#[cfg(not(target_arch = "wasm32"))]
fn dispatch(
    ui: &MainWindow,
    player: &Arc<Result<audio::Player, audio::AudioError>>,
    duration: &std::sync::atomic::AtomicI64,
    command: MediaCommand,
) {
    match command {
        MediaCommand::Next => {
            ui.global::<Player>().invoke_next_track()
        }
        MediaCommand::Previous => {
            ui.global::<Player>().invoke_prev_track()
        }
        MediaCommand::Play
        | MediaCommand::Pause
        | MediaCommand::Toggle => {
            if toggles(
                command,
                ui.global::<Player>().get_is_playing(),
            ) {
                ui.global::<Player>().invoke_toggle_play();
            }
        }
        MediaCommand::SetShuffle(_) => {
            if flips_shuffle(
                command,
                ui.global::<Player>().get_shuffle_on(),
            ) {
                ui.global::<Player>()
                    .invoke_shuffle_toggled();
            }
        }
        MediaCommand::SetLoop(_) => {
            if let Some(want) = wants_loop(
                command,
                loop_from_index(
                    ui.global::<Player>().get_loop_mode(),
                ),
            ) {
                ui.global::<Player>()
                    .invoke_loop_mode_set(loop_index(want));
            }
        }
        MediaCommand::SeekTo(_)
        | MediaCommand::SeekBy(_) => {
            let at = player
                .as_ref()
                .as_ref()
                .map(|player| {
                    player.position().as_millis() as i64
                })
                .unwrap_or_default();
            let Some(target) = seek_target(command, at)
            else {
                return;
            };
            let Some(ratio) = seek_ratio(
                target,
                duration.load(
                    std::sync::atomic::Ordering::Relaxed,
                ),
            ) else {
                return;
            };
            ui.global::<Player>().invoke_seek(ratio);
        }
    }
}

/// 把此刻在放的东西报给系统媒体控件。
///
/// 收 `playback` 与 `media` 而不是整个 `music::Deck`:取封面那个 future 只攥着这两样,
/// 为了推一次而把整个 deck 拖进闭包不值当。重复推是免费的,`Bridge` 自己去重。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn push(
    ui: &MainWindow,
    playback: &RefCell<Playback>,
    media: &Bridge,
) {
    let state = playback.borrow().state().clone();
    media.publish(NowPlaying::render(
        &state,
        ui.global::<Player>().get_is_playing(),
        ui.global::<Player>().get_shuffle_on(),
        loop_from_index(
            ui.global::<Player>().get_loop_mode(),
        ),
        media.art(),
    ));
}

#[cfg(test)]
mod tests;
