//! 一条用户播放意图的唯一出口:来源检查 → 目标选择 → 本机执行 / 发给服务端的组意图。
//!
//! 分三段,各归各的位置:
//!
//! - **translate** 在各个回调自己那里:连点去重、组装整批,产出一条 [`Intent`]。
//!   它只知道用户按了什么,不知道这一下会落在哪台设备上。
//! - **dispatch** 只有这一处([`dispatch`]):本机在组里就发给服务端(#142),不在就本机放。
//! - **execute** 是本机的执行体([`execute`]):命令落到本机播放器上。
//!
//! 组里的设备对等(#142):不管本机出不出声、是不是当初建组的那台,点歌、切歌、暂停、
//! 拖动都只改服务端的全局状态,出声的设备照状态收敛。没有「转发给哪一台」这条路。

use app_core::{Standing, TrackDto, TransportOpDto};

use super::*;
use crate::Player;
use crate::Shell;
use crate::music::*;

/// 用户在界面上按下的那一下,还没决定落在本机还是组上。
///
/// ⏯ 要结合当下在放没在放才知道是暂停还是继续,拖进度要按那一首的曲长换算,
/// 本机播放器已空时按播放是重播而不是继续 —— 翻译发生在 [`dispatch`] 里。
pub(in crate::music) enum Intent {
    /// 点一首歌:这一批成为队列,从第 `index` 首开始。
    ///
    /// 批次是**用户点中的那个列表** —— 他想听的是眼前这一批的后面那些歌。
    Play {
        tracks: Vec<TrackDto>,
        index: usize,
    },
    /// ⏯ 这一下。
    TogglePlay,
    Next,
    Prev,
    /// 拖进度条到这个比例。
    ///
    /// 带比例而不是毫秒:曲长取决于这一下落在哪 —— 只当遥控器时本机的
    /// `playback` 是空的,要按组里那一首算。
    Seek {
        ratio: f32,
    },
    Volume {
        level: f32,
    },
}

/// 一条意图的下场。
#[derive(Debug, PartialEq, Eq)]
pub(in crate::music) enum Dispatched {
    /// 落到本机播放器上了。
    LocalApplied,
    /// 发给服务端了(组意图)。成没成由应答与广播定,不由这个值定。
    GroupSubmitted,
    /// 规则挡下的:多余的连点、手上没歌可跳。
    Blocked(&'static str),
}

/// 本机执行的一条命令。
#[derive(Debug, Clone, PartialEq)]
pub(in crate::music) enum Command {
    Pause,
    Resume,
    Next,
    Prev,
    Seek { ms: u64 },
    Volume { level: f32 },
}

/// 本机 ⏯ 这一下到底是什么意思。
///
/// 抽成纯判断,理由与 `music::rules` 相同:它是最容易
/// 写反、也最难从截图上看出写反了的那一类,而起窗口测它还要一张声卡。
#[derive(Debug, PartialEq, Eq)]
pub(in crate::music) enum LocalToggle {
    Pause,
    Resume,
    /// 重播当前这首。
    Replay,
}

/// 差异 1:本机播放器**已经空了**的时候按播放是重播,不是 `Resume`。
///
/// 队列放完之后播放器里就没有源了,对着一个空播放器 resume 什么也不会发生 ——
/// 而用户按下去的时候,界面上明明还写着一首歌的名字。遥控那一侧没有这一档:
/// 那边只按被控端报来的 playing 与否翻成 `Pause`/`Resume`,因为「播放器空没空」
/// 是执行端自己的事,上报里根本没有这一位。
pub(in crate::music) const fn local_toggle(
    is_playing: bool,
    player_empty: bool,
) -> LocalToggle {
    if is_playing {
        LocalToggle::Pause
    } else if player_empty {
        LocalToggle::Replay
    } else {
        LocalToggle::Resume
    }
}

/// 本机播放器此刻在不在出声。没有播放器就是不在。
fn local_sounding(deck: &Deck) -> bool {
    deck.player.as_ref().as_ref().is_ok_and(is_sounding)
}

/// 一条用户播放意图的唯一出口。
pub(in crate::music) fn dispatch(
    ui: &MainWindow,
    deck: &Deck,
    intent: Intent,
) -> Dispatched {
    // 音量每台各自调(用户 2026-09-26),不进全局状态,在组里也落在本机。
    if deck.group.is_member()
        && !matches!(intent, Intent::Volume { .. })
    {
        to_group(ui, deck, intent)
    } else {
        to_local(ui, deck, intent)
    }
}

/// 本机在组里:翻成一条组意图发给服务端(#142)。
fn to_group(
    ui: &MainWindow,
    deck: &Deck,
    intent: Intent,
) -> Dispatched {
    let now = deck.group.now();
    let op = match intent {
        Intent::Play { tracks, index } => {
            deck.group.play(ui, tracks, index);
            return Dispatched::GroupSubmitted;
        }
        Intent::TogglePlay => {
            match now.as_ref().map(|now| now.playing) {
                Some(true) => TransportOpDto::Pause,
                Some(false) => TransportOpDto::Resume,
                None => {
                    return Dispatched::Blocked(
                        "组里还没有歌",
                    );
                }
            }
        }
        Intent::Next => TransportOpDto::Next,
        Intent::Prev => TransportOpDto::Prev,
        Intent::Seek { ratio } => {
            let Some(target) =
                now.as_ref().and_then(|now| {
                    crate::progress::seek_target(
                        ratio,
                        now.track.duration_ms,
                    )
                })
            else {
                return Dispatched::Blocked("没有在放的歌");
            };
            TransportOpDto::Seek {
                position_ms: target.as_millis() as u64,
            }
        }
        Intent::Volume { .. } => unreachable!("音量不进组"),
    };
    deck.group.transport(ui, op);
    Dispatched::GroupSubmitted
}

/// 只当遥控器、不出声的成员:本机播放器停着。
pub(in crate::music) fn is_silent_member(
    deck: &Deck,
) -> bool {
    deck.group.standing() == Standing::Remote
}

/// 不在组里(独奏):本机自己执行,行为与没有组时一样。
fn to_local(
    ui: &MainWindow,
    deck: &Deck,
    intent: Intent,
) -> Dispatched {
    match intent {
        Intent::Play { tracks, index } => {
            // 连点去重读的是**本机**的 playback,所以只在本机分支上问。
            let tapped =
                tracks.get(index).map(|track| &track.id);
            let redundant = tapped.is_some_and(|id| {
                is_redundant_tap(
                    deck.playback.borrow().state(),
                    id,
                    local_sounding(deck),
                )
            });
            if redundant {
                // 不挡的话,连点五下就是五条在途下载,每条回来都往播放器里
                // 塞一次源,声音从头响五遍;已经在响的那首则会被停掉、从头
                // 再加载一遍,还顺带多发布一次队列(#125)。
                return Dispatched::Blocked(
                    "这一下是多余的",
                );
            }
            // 先起播,再异步把这一批发布到服务端(`docs/adr/0031` 八):
            // 「所有播放都持久化」是目标,不是放歌的前置门槛 —— 服务端
            // 不可达时点不动歌是重大回退。
            // 同一批已经同步上去了:服务端那一版原样可用,不再发一个一模一样的
            // 新版本,只把「现在放到哪一条」记一笔(#137 ③)。
            let unchanged =
                deck.execution.identity().0.is_some()
                    && *deck.queue.borrow().tracks()
                        == tracks;
            play_batch(ui, deck, tracks.clone(), index);
            if unchanged {
                checkpoint(deck, index);
            } else {
                publish_local_queue(
                    ui, deck, tracks, index,
                );
            }
        }
        Intent::TogglePlay => {
            let Ok(player) = deck.player.as_ref() else {
                return Dispatched::Blocked("没有播放器");
            };
            match local_toggle(
                is_sounding(player),
                player.empty(),
            ) {
                LocalToggle::Pause => {
                    execute(ui, deck, Command::Pause);
                }
                LocalToggle::Resume => {
                    execute(ui, deck, Command::Resume);
                }
                LocalToggle::Replay => {
                    play_current(ui, deck);
                    crate::media::push(
                        ui,
                        &deck.playback,
                        &deck.media,
                    );
                }
            }
        }
        Intent::Next => execute(ui, deck, Command::Next),
        Intent::Prev => execute(ui, deck, Command::Prev),
        Intent::Seek { ratio } => {
            // 按本机正在放的那一首算曲长。手上没歌就没什么可跳的。
            let state =
                deck.playback.borrow().state().clone();
            let (PlaybackState::Playing(track)
            | PlaybackState::Loading(track)) = state
            else {
                return Dispatched::Blocked("没有在放的歌");
            };
            let Some(target) = crate::progress::seek_target(
                ratio,
                track.duration_ms,
            ) else {
                return Dispatched::Blocked(
                    "这一首没有时长",
                );
            };
            execute(
                ui,
                deck,
                Command::Seek {
                    ms: target.as_millis() as u64,
                },
            );
        }
        Intent::Volume { level } => {
            execute(ui, deck, Command::Volume { level });
        }
    }
    Dispatched::LocalApplied
}

/// 本机点播之后,把这一批异步发布成服务端队列。
///
/// **不挡播放**:声音已经出来了,这一趟只是给它安一个 `queue_id`。失败就把
/// 执行副本标成「还没同步上去」,界面据此说一句,恢复之后下一次点播再对账
/// (`docs/adr/0031` 八)。
///
/// 这条路径与**被接管时把本机队列注册上去**(AC-9)是**同一条** —— 接管那一刻
/// 本机手上这批要么早就有 `queue_id`(这里发布成功过),要么没有(这里失败过),
/// 后者补发一次走的还是这个函数。不写成两套。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn publish_local_queue(
    ui: &MainWindow,
    deck: &Deck,
    tracks: Vec<TrackDto>,
    index: usize,
) {
    // 超出配额的不必往返一次才知道 —— 而且它照样在本机放着,只是同步不上去。
    if tracks.len() > api::MAX_QUEUE_ENTRIES {
        deck.execution.detach();
        mark_sync(ui, deck);
        return;
    }

    deck.execution
        .note_publish(crate::sync::group::now_ms());

    let device = crate::sync::link::local_device_id();
    // 已经有这台设备的队列就**发新版本**,不是再建一个。
    //
    // 每点一次歌建一个的话,一天下来几百个队列,而账号的队列数是有上限的
    // (`store::queue::MAX_QUEUES_PER_ACCOUNT`)—— 更要紧的是那不对:
    // 队列归**播放会话 / 输出设备**,这台设备就该只有一个当前队列
    // (`docs/adr/0031` 二)。
    let held = deck.execution.identity();
    let tracks_len = tracks.len();
    let deck = deck.clone();
    let weak = ui.as_weak();
    let _ = slint::spawn_local(async move {
        let published = match held {
            (Some(queue_id), _, Some(applied)) => {
                let outcome = api::publish_queue(
                    queue_id,
                    applied,
                    tracks.clone(),
                )
                .await;
                match outcome {
                    // 版本被别人推进过:这台设备的队列不该有别人在改,
                    // 真撞上就重新建一个,而不是拿一个猜的版本号硬覆盖。
                    Err(api::ApiError::Server {
                        ref code,
                        ..
                    }) if code == "revision_conflict" => {
                        log::warn!(
                            "本机队列的版本被改过,另建一个"
                        );
                        api::create_queue(&device, tracks)
                            .await
                    }
                    other => other,
                }
            }
            _ => api::create_queue(&device, tracks).await,
        };
        let Some(ui) = weak.upgrade() else { return };

        match published {
            Ok(reference) => {
                // 条目号就在应答里,按位置排 —— 不再把整份队列读回来(#137 ③)。
                if reference.entry_ids.len() == tracks_len {
                    deck.execution.adopt(
                        reference.queue_id,
                        reference.revision,
                        reference.entry_ids,
                    );
                    checkpoint(&deck, index);
                } else {
                    log::warn!(
                        "队列发布了,但应答里的条目号是 {} 条、这一批是 {tracks_len} 首",
                        reference.entry_ids.len()
                    );
                    deck.execution.detach();
                }
            }
            Err(error) => {
                // 不挡播放,只记一笔。
                log::warn!("本机队列没同步上去: {error}");
                deck.execution.detach();
            }
        }
        mark_sync(&ui, &deck);
    });
}

/// 服务端回来了就把没同步上去的那一批补提交(AC-12 的「恢复后对账」)。
///
/// 每秒那趟轮询叫它一次,但真正发出去由 `due_for_resync` 节流 —— 服务端
/// 不可达时每秒打一发,日志会被刷满,而它恢复的时刻不由我们决定。
///
/// 只在**独奏、手上有歌、而且还没拿到 `queue_id`** 时才动:在组里时放的是组队列,
/// 本机这边不该去抢着发布。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn resync_local_queue(
    ui: &MainWindow,
    deck: &Deck,
) {
    if deck.group.is_member() {
        return;
    }
    if deck.queue.borrow().tracks().is_empty() {
        return;
    }
    // 先判节流再拷队列:这一趟每秒都来,而几千首的整份拷贝多数时候是白拷(#137 ⑥)
    if !deck
        .execution
        .due_for_resync(crate::sync::group::now_ms())
    {
        return;
    }
    let (tracks, index) = {
        let queue = deck.queue.borrow();
        (queue.tracks().to_vec(), queue.index())
    };

    log::info!("本机队列还没同步上去,补提交一次");
    publish_local_queue(ui, deck, tracks, index);
}

/// 把「这一批同步上去没有」推到界面上。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn mark_sync(
    ui: &MainWindow,
    deck: &Deck,
) {
    let (queue_id, ..) = deck.execution.identity();
    ui.global::<Shell>()
        .set_queue_unsynced(queue_id.is_none());
}

/// 把这一刻的执行状态作为检查点存到服务端。
///
/// **不是每秒一次**:每秒那条走信令给遥控器看(小状态),这一条是给服务端
/// 留的恢复检查点,只在事件上发 —— 换了批、换了歌、洗了牌、回卷。排列只在
/// 它真的变了时才带(见 `Execution::order_to_report`),否则每秒就是几千个
/// bigint 的重写,而线上字节数并不会涨。
///
/// **自动下一首不等它落库**(`docs/adr/0031` 四):这里发出去就不管了,
/// 失败只留一行日志 —— 断连时后端那份本来就是旧的,而它被标成过期。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn checkpoint(
    deck: &Deck,
    index: usize,
) {
    let (Some(queue_id), _, Some(applied_revision)) =
        deck.execution.identity()
    else {
        // 还没同步上去的那一批没有检查点可存,这是正常状态。
        return;
    };

    let (order, round, position_ms) = {
        let queue = deck.queue.borrow();
        let order: Vec<i64> = queue
            .order()
            .iter()
            .filter_map(|at| deck.execution.entry_at(*at))
            .collect();
        (order, queue.round(), 0)
    };
    let play_order = deck.execution.order_to_report(&order);
    let (epoch, state_seq) = deck.execution.stamp();
    let report = api::QueueReportDto {
        device_id: crate::sync::link::local_device_id(),
        epoch,
        state_seq: state_seq as i64,
        applied_revision,
        entry_id: deck.execution.entry_at(index),
        play_order,
        round: round as i64,
        position_ms,
        state: app_core::RemotePlayState::Playing,
        operation: None,
    };

    let _ = slint::spawn_local(async move {
        if let Err(error) =
            api::report_queue_state(queue_id, report).await
        {
            // 丢一条检查点不影响放歌:它只是让服务端手上那份新一点。
            log::debug!("检查点没送到,下一次再说: {error}");
        }
    });
}

/// 把一整批曲目装进队列并起播。
///
/// 独奏时本机点播落到这里;自动续播仍在这一端发生。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn play_batch(
    ui: &MainWindow,
    deck: &Deck,
    tracks: Vec<TrackDto>,
    index: usize,
) {
    // 只动队列,不动浏览视图:眼前那页列表是用户在看的来源,不是队列的镜子
    // (#137 ④)。
    // replace 把随机清掉(新批还没洗过),开着的话补洗一次把它立回去。
    // 开没开问队列自己,不回读界面上那个开关 —— 开关是它的投影。
    let shuffled = deck.queue.borrow().is_shuffled();
    deck.queue.borrow_mut().replace(tracks, index);
    if shuffled {
        deck.queue.borrow_mut().shuffle(shuffle_seed());
    }
    play_current(ui, deck);
    crate::media::push(ui, &deck.playback, &deck.media);
}

/// 音量停手多久之后才写盘(#137 ⑥)。
pub(in crate::music) const VOLUME_SAVE_DELAY:
    core::time::Duration =
    core::time::Duration::from_millis(400);

/// 音量的节流存盘:拖滑块是一串连着的命令,每动一下都同步读写一次设置文件
/// 就是 UI 线程上每帧一次磁盘 IO。每动一下只记住值、把钟往后拨,停手
/// [`VOLUME_SAVE_DELAY`] 之后写一次最后那个值。
///
/// ponytail: 停手不到 0.4 秒就退出进程,最后那一下没写进去;真在意时在退出路径上 flush。
#[derive(Clone, Default)]
pub(in crate::music) struct VolumeSave {
    timer: Rc<slint::Timer>,
    level: Rc<std::cell::Cell<f32>>,
}

impl VolumeSave {
    pub(in crate::music) fn remember(&self, level: f32) {
        self.level.set(level);
        let level = self.level.clone();
        self.timer.start(
            slint::TimerMode::SingleShot,
            VOLUME_SAVE_DELAY,
            move || {
                // **先读再改**:整份重造的话,这个文件里别的设置(明暗)会被
                // 这次调音量顺手冲回默认值。
                api::settings::save(
                    &api::settings::Settings {
                        volume: level.get(),
                        ..api::settings::load()
                    },
                );
            },
        );
    }
}

/// 在本机执行一条命令:本机用户动作(经 [`dispatch`] 的独奏分支)与自动续播共用。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn execute(
    ui: &MainWindow,
    deck: &Deck,
    cmd: Command,
) {
    match cmd {
        Command::Pause => {
            if let Ok(player) = deck.player.as_ref() {
                player.pause();
            }
            ui.global::<Player>().set_is_playing(false);
        }
        Command::Resume => {
            if let Ok(player) = deck.player.as_ref() {
                player.resume();
            }
            ui.global::<Player>().set_is_playing(true);
        }
        Command::Next => advance(ui, deck),
        Command::Prev => {
            if deck.queue.borrow_mut().previous().is_some()
            {
                play_current(ui, deck);
            }
        }
        Command::Seek { ms } => {
            // 立刻挂上「缓冲中」;当场就知道跳不动的当场说。
            ui.global::<Player>().set_buffering(true);
            if let Ok(player) = deck.player.as_ref()
                && let Err(err) = player.seek(
                    core::time::Duration::from_millis(ms),
                )
            {
                ui.global::<Player>().set_buffering(false);
                crate::notice::show(
                    ui,
                    format!("这首跳不了: {err}"),
                );
            }
        }
        Command::Volume { level } => {
            let level = audio::clamped_volume(level);
            if let Ok(player) = deck.player.as_ref() {
                player.set_volume(level);
            }
            ui.global::<Player>().set_volume(level);
            // 音量跟着设备走,不跟着账号:记住这个数的是这台机器。
            deck.volume_save.remember(level);
        }
    }
    crate::media::push(ui, &deck.playback, &deck.media);
}
