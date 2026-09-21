//! 一条用户播放意图的唯一出口:来源检查 → 目标选择 → 本机执行 / 远端提交。
//!
//! 在这一层出现之前,两道闸(被遥控时本机动作不生效 / 输出不在本机时改发命令)
//! 在六个回调入口各写了一遍,而且写法并不一致 —— 有三处忽略提交结果,另外三处
//! 拿提交结果决定要不要回落本机,还有一处压根没有第一道闸。漏掉的那个在界面上
//! 表现为「别的键都遥控,唯独这个还在本机上放」(#108)。
//!
//! 分三段,各归各的位置:
//!
//! - **translate** 在各个回调自己那里:连点去重、组装整批,产出一条 [`Intent`]。
//!   它只知道用户按了什么,不知道这一下会落在哪台设备上。
//! - **dispatch** 只有这一处([`dispatch`]):谁能按、按给谁、翻成什么。
//! - **execute** 是与来源无关的共享执行体([`execute`]):命令落到本机播放器上。
//!   遥控器发来的命令**直接**进它,不经过 [`dispatch`] —— 进去的话这台会把
//!   收到的命令再转发回去。
//!
//! 路由按**已选目标**([`app_core::Output`])走,不按提交结果的那个布尔值。
//! 这是与改之前最要紧的一处不同:目标在别的设备而提交失败时,声音不回落本机,
//! 而是保留目标并说一句「控制暂不可用」(`docs/adr/0030`)。

use app_core::{RemoteCommand, TrackDto};

use super::*;
use crate::Player;
use crate::music::*;
use crate::sync::remote::Submitted;

/// 用户在界面上按下的那一下,还没决定落在哪台设备上。
///
/// 不直接用 [`RemoteCommand`]:线上那个类型表达的是「已经定下来发给被控端的
/// 一条命令」,而这里有几样它说不了 —— ⏯ 要结合**所选目标**当下的状态才知道
/// 是暂停还是继续,拖进度要按目标端报的曲长换算,本机播放器已空时按播放是
/// 重播而不是 `Resume`。翻译发生在 [`dispatch`] 里,线上的契约这一轮不加字段。
pub(in crate::music) enum Intent {
    /// 点一首歌:这一批成为队列,从第 `index` 首开始。
    ///
    /// 批次是**用户点中的那个列表**,不是被控端回报的队列 —— 他想听的是眼前
    /// 这一批的后面那些歌。
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
    /// 带比例而不是毫秒:目标曲长取决于这一下落在哪一端 —— 遥控时本机的
    /// `playback` 是空的,拿它算只会得到 `None`,而进度条看起来就是拖不动。
    Seek {
        ratio: f32,
    },
    Volume {
        level: f32,
    },
}

/// 一条意图的下场。
///
/// 四个而不是一个布尔:调用方要能分清「这一下被规则挡了」与「目标不可用」——
/// 前者是本该如此,后者要给用户一句话。
#[derive(Debug, PartialEq, Eq)]
pub(in crate::music) enum Dispatched {
    /// 落到本机播放器上了。
    LocalApplied,
    /// **本地提交**成功 —— 仅此而已。
    ///
    /// 队列、服务端转发、被控端执行都还在后面,任何一跳都可能悄悄丢掉它
    /// (`Client::command` 只把命令塞进一条通道)。真放起来了以被控端的
    /// 上报为准,不以这个值为准。
    RemoteSubmitted,
    /// 规则挡下的:本机正被遥控、或者这一下是多余的连点。
    Blocked(&'static str),
    /// 目标在别的设备,但这一下没送出去。**不回落本机**。
    Unavailable(&'static str),
}

impl Intent {
    /// 这一下受不受「本机正被遥控」那道锁的限制。
    ///
    /// 音量不受限(产品裁决,#108 实施计划第 6 条):那道锁拦的是 transport ——
    /// 放什么、放不放,因为遥控器那头正按着这台报来的进度插值,本机偷偷改一下
    /// 就会让对面的进度条撒谎。音量不在那条链上:它是这台机器的响度,被控端
    /// 前面的人伸手拧一下自己音箱是物理动作,而且音量每秒随快照报一次,
    /// 最迟一秒后遥控器就看见了,谈不上骗谁。
    ///
    /// 这也**保持**了改之前的行为:`bind_volume` 本来就没有这道闸。在一次
    /// 重构里无声地给它加上,等于借收口之名改产品规则。
    const fn obeys_controlled_lock(&self) -> bool {
        !matches!(self, Self::Volume { .. })
    }

    /// 这一下之前要不要先退出收听,以及退出之后还做不做后面那段。
    ///
    /// 各命令不同,不能一把 `leave()` 盖全部(`CONTEXT.md`「听众」):
    /// 点歌与切歌退出后**继续**作用于本机队列 —— 点了「下一首」的人想听的是
    /// 自己的下一首,不是单纯安静下来;⏯ 退出即止(退出即静音,再按才操作
    /// 自己的队列);音量允许听众一边听一边调自己的响度;拖进度作用在那路
    /// 收来的流上,不是退出的理由。
    const fn leaving_rule(&self) -> Leaving {
        match self {
            Self::Play { .. } | Self::Next | Self::Prev => {
                Leaving::ThenContinue
            }
            Self::TogglePlay => Leaving::AndStop,
            Self::Seek { .. } | Self::Volume { .. } => {
                Leaving::Stay
            }
        }
    }
}

/// 本机 ⏯ 这一下到底是什么意思。
///
/// 抽成纯判断,理由与 `music::rules`、`sync::remote::rules` 相同:它是最容易
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

/// 收听中按下这一下时,退出收听的三种规矩。
enum Leaving {
    /// 先退出,然后照常往下做。
    ThenContinue,
    /// 退出即止:按停、不再往下做。
    AndStop,
    /// 不退出。
    Stay,
}

/// 一条用户播放意图的唯一出口。
pub(in crate::music) fn dispatch(
    ui: &MainWindow,
    deck: &Deck,
    intent: Intent,
) -> Dispatched {
    // ── 来源检查 ──
    // 锁只拦**本机用户动作**。遥控器发来的命令走 `bind_remote` 直接进
    // `execute`,自动续播走 `advance_auto` —— 两者都不经过这里,否则遥控器
    // 一锁屏,被控端放完一首就再也接不上下一首。
    if deck.remote.is_controlled()
        && intent.obeys_controlled_lock()
    {
        return Dispatched::Blocked("本机正被遥控");
    }

    // ── 目标选择 ──
    // 判据是**已选目标**,不是提交结果。
    if deck.remote.is_remote() {
        to_remote(ui, deck, intent)
    } else {
        to_local(ui, deck, intent)
    }
}

/// 目标在别的设备:翻成一条命令交出去,本机一声不出。
fn to_remote(
    ui: &MainWindow,
    deck: &Deck,
    intent: Intent,
) -> Dispatched {
    // 点播要先把这一批发布成服务端队列、拿到 queue_id/revision 才发得出
    // 命令(`docs/adr/0031`)。契约这一轮切完,发布那一段是 #109 第 4 段。
    // 在这里明确拒绝而不是让它默默失败:静默的话用户按下去毫无反应,
    // 而那与「队列太长」「控制暂不可用」在界面上分不开。
    if matches!(intent, Intent::Play { .. }) {
        crate::notice::show(
            ui,
            "远端点播正在改造中,这一版还发不出去(#109)"
                .to_owned(),
        );
        return Dispatched::Unavailable(
            "队列还没接上服务端",
        );
    }

    // 退出规矩先问,因为下一行就把意图交出去了。它只看变体,不看载荷。
    let leaving = intent.leaving_rule();

    let Some(cmd) = as_command(ui, deck, intent) else {
        // 翻不出命令只有一种情形:遥控时拖进度,而被控端报来的那份还没有
        // 曲目(刚接管、或者对面没在放)。这一下没有可发的东西。
        return refuse(ui, deck, Submitted::Stale);
    };

    match deck.remote.send(cmd) {
        Submitted::Ok => {}
        outcome => return refuse(ui, deck, outcome),
    }

    // 交出去之后才退出收听:一条没送出去的命令不该顺手把用户正在听的那路
    // 流也拆掉。规矩与本机路径同一份 —— 点歌与切歌是「这台不再收流了」,
    // ⏯ 与音量不是。
    match leaving {
        Leaving::ThenContinue => leave_listening(deck),
        Leaving::AndStop | Leaving::Stay => {}
    }
    Dispatched::RemoteSubmitted
}

/// 没发出去:说一句**对得上原因**的话,并**保留**当前目标。
///
/// 改之前这里是 `if send(..) { return }` 然后径直落到本机 —— 那正是要改掉的
/// 那一半:状态过期时按下一首,声音会从遥控器自己这台放出来(`docs/adr/0030`)。
///
/// 两种原因要说两句话,因为出路相反:「控制暂不可用」等一等就好了,
/// 「队列太长」等多久都不会好,得换一个短一点的列表(根治见 #109)。
fn refuse(
    ui: &MainWindow,
    deck: &Deck,
    outcome: Submitted,
) -> Dispatched {
    match outcome {
        Submitted::TooLarge { .. } => {
            crate::notice::show(
                ui,
                deck.remote.too_large_notice(),
            );
            Dispatched::Unavailable("这一批太长,发不出去")
        }
        _ => {
            crate::notice::show(
                ui,
                deck.remote.unavailable_notice(),
            );
            Dispatched::Unavailable("目标此刻收不了命令")
        }
    }
}

/// 把意图翻成一条发给被控端的命令。
///
/// ⏯ 按界面当下画的是 ⏸ 还是 ▶ 来定 —— 那个图标读的正是被控端报来的状态,
/// 所以它就是对的那个判据。
///
/// 收所有权而不是借用:`Play` 拖着整批曲目,借用就得把它整个克隆一遍,
/// 而那正是这条链上最大的那份数据。
fn as_command(
    ui: &MainWindow,
    deck: &Deck,
    intent: Intent,
) -> Option<RemoteCommand> {
    Some(match intent {
        // 点播在 `to_remote` 的入口就被挡下了(见那里):它要先发布队列。
        Intent::Play { .. } => return None,
        Intent::TogglePlay => {
            if ui.global::<Player>().get_is_playing() {
                RemoteCommand::Pause
            } else {
                RemoteCommand::Resume
            }
        }
        Intent::Next => RemoteCommand::Next,
        Intent::Prev => RemoteCommand::Prev,
        Intent::Seek { ratio } => {
            // 按**被控端报来的**曲长算:本机的 playback 此刻是空的。
            let target =
                deck.remote.with_view(|view, _| {
                    view.track().and_then(|track| {
                        crate::progress::seek_target(
                            ratio,
                            track.duration_ms,
                        )
                    })
                })?;
            RemoteCommand::Seek {
                ms: target.as_millis() as u64,
            }
        }
        Intent::Volume { level } => {
            RemoteCommand::Volume { level }
        }
    })
}

/// 输出在本机:自己执行,不发信令。
///
/// 「本机播放也应该当作自己控制自己」—— 除了下面逐条注明的几处,本机分支与
/// 收到一条遥控命令走的是同一段 [`execute`]。**不是**全盘等同:那几处差异
/// 是真实的产品行为,统一掉会悄悄改变用户看得见的东西。
fn to_local(
    ui: &MainWindow,
    deck: &Deck,
    intent: Intent,
) -> Dispatched {
    match intent.leaving_rule() {
        Leaving::ThenContinue => leave_listening(deck),
        Leaving::AndStop => {
            if deck.sync.is_listening() {
                deck.sync.leave();
                if let Ok(player) = deck.player.as_ref() {
                    player.stop();
                }
                ui.global::<Player>().set_is_playing(false);
                return Dispatched::LocalApplied;
            }
        }
        Leaving::Stay => {}
    }

    match intent {
        Intent::Play { tracks, index } => {
            // 差异 5:连点去重读的是**本机**的 playback,所以只在本机分支上
            // 问。早于目标选择去问的话,转为遥控时会拿本机残留的 `Loading`
            // 把一条本该发出去的远端意图丢掉。
            let tapped =
                tracks.get(index).map(|track| &track.id);
            let redundant = tapped.is_some_and(|id| {
                is_redundant_tap(
                    deck.playback.borrow().state(),
                    id,
                )
            });
            if redundant {
                // 不挡的话,连点五下就是五条在途下载,每条回来都往播放器里
                // 塞一次源,声音从头响五遍。
                return Dispatched::Blocked(
                    "这一下是多余的",
                );
            }
            play_batch(ui, deck, tracks, index);
        }
        Intent::TogglePlay => {
            let Ok(player) = deck.player.as_ref() else {
                return Dispatched::Blocked("没有播放器");
            };
            match local_toggle(
                ui.global::<Player>().get_is_playing(),
                player.empty(),
            ) {
                LocalToggle::Pause => {
                    execute(ui, deck, RemoteCommand::Pause);
                }
                LocalToggle::Resume => {
                    execute(
                        ui,
                        deck,
                        RemoteCommand::Resume,
                    );
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
        Intent::Next => {
            execute(ui, deck, RemoteCommand::Next)
        }
        Intent::Prev => {
            execute(ui, deck, RemoteCommand::Prev)
        }
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
                RemoteCommand::Seek {
                    ms: target.as_millis() as u64,
                },
            );
        }
        Intent::Volume { level } => {
            execute(
                ui,
                deck,
                RemoteCommand::Volume { level },
            );
        }
    }
    Dispatched::LocalApplied
}

/// 正在收听同播就退出。点歌与切歌之后还要接着作用于本机队列。
fn leave_listening(deck: &Deck) {
    if deck.sync.is_listening() {
        deck.sync.leave();
    }
}

/// 把一整批曲目装进队列并起播。
///
/// 从 [`execute`] 里拆出来,因为**曲目不再随命令过来**(`docs/adr/0031`):
/// 线上那条 `Play` 只带队列标识,而这一段是「拿到了曲目之后做什么」。
/// 两个来源共用它 —— 本机点播手上本来就有这一批;遥控点播要先按
/// `queue_id`/`revision` 把执行副本取下来(#109 第 4 段),取到之后落到这里。
///
/// 自动续播仍然在这一端发生:装进来的是整批,发命令的那头锁屏、断线都不该
/// 让这边停在一首上。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn play_batch(
    ui: &MainWindow,
    deck: &Deck,
    tracks: Vec<TrackDto>,
    index: usize,
) {
    deck.tracks.borrow_mut().clone_from(&tracks);
    deck.queue.borrow_mut().replace(tracks, index);
    // replace 把随机清掉(新批还没洗过),开着的话补洗一次把它立回去。
    if ui.global::<Player>().get_shuffle_on() {
        deck.queue.borrow_mut().shuffle(shuffle_seed());
    }
    play_current(ui, deck);
    crate::media::push(ui, &deck.playback, &deck.media);
}

/// 执行一条命令,**不问它是从哪来的**。
///
/// 三个来源共用这一段:遥控器发来的命令(`bind_remote`)、本机用户动作
/// (经 [`dispatch`] 的本机分支)、以及自动续播。"本机播放也应该当作自己
/// 控制自己" 指的就是这一层 —— 共用命令执行语义,但本机**不**因此建立一个
/// 自己遥控自己的 `ControlledBy` 会话。
///
/// 改名自 `apply_remote`:名字里带 remote 的话,谁也不好意思从本机那条路
/// 调它,于是本机那半边又会各写一遍。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn execute(
    ui: &MainWindow,
    deck: &Deck,
    cmd: RemoteCommand,
) {
    match cmd {
        RemoteCommand::Play {
            queue_id,
            revision,
            entry_id,
            operation_id,
        } => {
            // 曲目不再随命令过来(`docs/adr/0031`):这一条只说「播服务端
            // 那个队列的这一条」,执行副本要按 queue_id/revision 经 HTTP
            // 自己去取,校验完再原子替换。
            //
            // 取数那一段是 #109 第 4 段的活。契约这一轮先切,两端的取数与
            // 原子替换在下一段接上 —— 在那之前遥控点播是不通的,而不通时
            // 出声比不出声更糟(会放出一首没点过的歌)。
            log::warn!(
                "收到 play(队列 {queue_id}@{revision}, 条目 {entry_id}, \
                 操作 {operation_id}),但取数那一段还没接上(#109 第 4 段)"
            );
        }
        RemoteCommand::Pause => {
            if let Ok(player) = deck.player.as_ref() {
                player.pause();
            }
            ui.global::<Player>().set_is_playing(false);
        }
        RemoteCommand::Resume => {
            if let Ok(player) = deck.player.as_ref() {
                player.resume();
            }
            ui.global::<Player>().set_is_playing(true);
        }
        RemoteCommand::Next => advance(ui, deck),
        RemoteCommand::Prev => {
            if deck.queue.borrow_mut().previous().is_some()
            {
                play_current(ui, deck);
            }
        }
        RemoteCommand::Seek { ms } => {
            // 差异 2:立刻挂上「缓冲中」、当场失败就当场说。改之前只有本机
            // 那条路这么做,遥控命令这条把 `seek` 的错误丢掉了 —— 于是被控端
            // 自己的界面上,一次跳不动的跳转要么毫无反应、要么永远停在缓冲上。
            // 两条路归一之后按**本机那一份**来:执行的是哪台设备,就该由哪台
            // 设备的界面说话(`docs/adr/0019`:裁决由被控端回)。
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
        RemoteCommand::Volume { level } => {
            let level = audio::clamped_volume(level);
            if let Ok(player) = deck.player.as_ref() {
                player.set_volume(level);
            }
            ui.global::<Player>().set_volume(level);
            // 差异 3:存盘归**设备执行**这一侧,不归「本机用户操作」。
            // 判据是那句「音量跟着设备走,不跟着账号」—— 真正改变响度的是
            // 这台机器的播放器,那么记住这个数的也该是这台机器,不管拧旋钮
            // 的手是本机用户的还是遥控器的。改之前只有本机那条路存,于是
            // 遥控器把被控端调小之后,被控端一重启就跳回原来的音量。
            //
            // **先读再改**:整份重造的话,这个文件里别的设置(明暗)会被
            // 这次调音量顺手冲回默认值。
            api::settings::save(&api::settings::Settings {
                volume: level,
                ..api::settings::load()
            });
        }
    }
    crate::media::push(ui, &deck.playback, &deck.media);
}
