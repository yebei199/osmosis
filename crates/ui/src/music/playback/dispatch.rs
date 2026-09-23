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
use crate::Shell;
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
    // 点播要先把这一批**发布成服务端队列**,拿到 queue_id/revision 才发得出
    // 命令(`docs/adr/0031`)—— 曲目不再随命令走信令。发布是一次 HTTP 往返,
    // 所以这一条与别的命令不同,不在这里当场发完。
    if let Intent::Play { tracks, index } = intent {
        return submit_remote_play(ui, deck, tracks, index);
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

/// 遥控器侧的点播:把用户眼前这一批冻结成服务端队列,再发一条只带标识的命令。
///
/// 三步都在一次 `spawn_local` 里,因为它们是一件事的三段,中间断在哪里都
/// 不该留下「队列建了但没人播」:
///
/// 1. `POST /queues` —— 冻结的是**用户实际看到并选择的有序条目**,不是一个
///    会被重跑的查询(`docs/adr/0031` 五)。队列归**目标设备**的播放会话,
///    不是遥控器自己这台。
/// 2. `POST /queues/{id}/intent` —— 让这一下**先落库**。WebSocket 那条通知
///    丢了、或者服务端随后重启,播放端恢复时读 head 仍然对得上账
///    (`docs/adr/0031` 七)。
/// 3. 发 `RemoteCommand::Play` —— 只是把播放端叫醒,不是唯一的送达手段。
///
/// 返回 [`Dispatched::RemoteSubmitted`] 的时机与别的命令一致:它说的一直都是
/// 「**本地**交出去了」,后面每一跳都可能丢掉它,真放起来了以被控端的上报为准。
#[cfg(not(target_arch = "wasm32"))]
fn submit_remote_play(
    ui: &MainWindow,
    deck: &Deck,
    tracks: Vec<TrackDto>,
    index: usize,
) -> Dispatched {
    let Some(target) = deck.remote.target_id() else {
        return refuse(ui, deck, Submitted::NotRemote);
    };
    // 超出约定规模**当场**拒绝,不等那次 HTTP 往返回来:用户要的是一句立刻
    // 出现的话,而这一条等多久都不会好(AC-6)。
    if tracks.len() > api::MAX_QUEUE_ENTRIES {
        crate::notice::show(
            ui,
            deck.remote.too_large_notice(),
        );
        return Dispatched::Unavailable("这一批太长");
    }

    deck.remote.note_play_submitted();

    let deck = deck.clone();
    let weak = ui.as_weak();
    let _ = slint::spawn_local(async move {
        let outcome = publish_and_command(
            &deck, &target, tracks, index,
        )
        .await;
        if let Err(why) = outcome
            && let Some(ui) = weak.upgrade()
        {
            crate::notice::show(&ui, why);
        }
    });

    Dispatched::RemoteSubmitted
}

/// 上面那三步的正身。失败给一句**给人看的**话。
#[cfg(not(target_arch = "wasm32"))]
async fn publish_and_command(
    deck: &Deck,
    target: &str,
    tracks: Vec<TrackDto>,
    index: usize,
) -> Result<(), String> {
    let published = api::create_queue(target, tracks)
        .await
        .map_err(describe_publish_failure)?;

    // 条目号要从服务端读回来:发布那一刻服务端才给号,而队列允许同一首歌
    // 出现多次 —— 拿下标去猜会在重复项上指错一条。
    let entries = api::fetch_queue(
        published.queue_id,
        published.revision,
    )
    .await
    .map_err(describe_publish_failure)?;
    let entry_id = entries
        .get(index)
        .map(|entry| entry.entry_id)
        .ok_or_else(|| {
            "服务端收下的队列里没有点的那一首".to_owned()
        })?;

    let operation_id = fresh_operation_id();
    api::set_queue_intent(
        published.queue_id,
        api::SetQueueIntentDto {
            device_id: target.to_owned(),
            revision: published.revision,
            entry_id,
            operation_id: operation_id.clone(),
        },
    )
    .await
    .map_err(describe_publish_failure)?;

    match deck.remote.send(RemoteCommand::Play {
        queue_id: published.queue_id,
        revision: published.revision,
        entry_id,
        operation_id,
    }) {
        Submitted::Ok => Ok(()),
        // 命令没送出去**不等于**这一下白点了:意图已经落库,播放端下一次
        // 读 head 就会看到它。所以这里只说一句,不回滚队列。
        _ => Err(deck.remote.unavailable_notice()),
    }
}

/// 发布失败时给人看的那句话。
///
/// 两种分开说,因为出路相反:超限要换一个短点的列表,别的等一等再来。
#[cfg(not(target_arch = "wasm32"))]
fn describe_publish_failure(
    error: api::ApiError,
) -> String {
    match &error {
        api::ApiError::Server { code, .. }
            if code == "queue_too_large" =>
        {
            format!(
                "这一批太长,最多 {} 首",
                api::MAX_QUEUE_ENTRIES
            )
        }
        _ => format!("队列没能同步到服务端: {error}"),
    }
}

/// 一次操作的标识。
///
/// 时间加一个进程内自增数:要的只是「这一次与上一次不是同一次」,而重试同
/// 一次点播时调用方会把同一个值再用一遍。不引 uuid —— 换不来更少的代码。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn fresh_operation_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "{}-{}",
        crate::sync::remote::now_ms(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
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
                    ui.global::<Player>().get_is_playing(),
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
            play_batch(ui, deck, tracks.clone(), index);
            publish_local_queue(ui, deck, tracks, index);
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
        .note_publish(crate::sync::remote::now_ms());

    let device = crate::sync::syncplay::local_device_id();
    // 已经有这台设备的队列就**发新版本**,不是再建一个。
    //
    // 每点一次歌建一个的话,一天下来几百个队列,而账号的队列数是有上限的
    // (`store::queue::MAX_QUEUES_PER_ACCOUNT`)—— 更要紧的是那不对:
    // 队列归**播放会话 / 输出设备**,这台设备就该只有一个当前队列
    // (`docs/adr/0031` 二)。
    let held = deck.execution.identity();
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
                // 条目号要读回来:服务端发号,而队列允许重复项,拿下标猜
                // 会在重复的那几条上指错一条。
                let entries = api::fetch_queue(
                    reference.queue_id,
                    reference.revision,
                )
                .await;
                match entries {
                    Ok(entries) => {
                        deck.execution.adopt(
                            reference.queue_id,
                            reference.revision,
                            entries
                                .iter()
                                .map(|entry| entry.entry_id)
                                .collect(),
                        );
                        checkpoint(&deck, index);
                    }
                    Err(error) => {
                        log::warn!(
                            "队列发布了但条目号没读回来: {error}"
                        );
                        deck.execution.detach();
                    }
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
/// 只在**输出在本机、手上有歌、而且还没拿到 `queue_id`** 时才动:遥控时
/// 那份队列归被控端管,本机这边不该去抢着发布。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn resync_local_queue(
    ui: &MainWindow,
    deck: &Deck,
) {
    if deck.remote.is_remote()
        || deck.remote.is_controlled()
    {
        return;
    }
    let (tracks, index) = {
        let queue = deck.queue.borrow();
        (queue.tracks().to_vec(), queue.index())
    };
    if tracks.is_empty() {
        return;
    }
    if !deck
        .execution
        .due_for_resync(crate::sync::remote::now_ms())
    {
        return;
    }

    log::info!("本机队列还没同步上去,补提交一次");
    publish_local_queue(ui, deck, tracks, index);
}

/// 把「这一批同步上去没有」推到界面上。
#[cfg(not(target_arch = "wasm32"))]
fn mark_sync(ui: &MainWindow, deck: &Deck) {
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
    let (epoch, state_seq) = deck.remote.stamp();
    let report = api::QueueReportDto {
        device_id: crate::sync::syncplay::local_device_id(),
        epoch,
        state_seq: state_seq as i64,
        applied_revision,
        entry_id: deck.execution.entry_at(index),
        play_order,
        round: round as i64,
        position_ms,
        state: app_core::RemotePlayState::Playing,
        operation: deck.execution.take_outcome().map(
            |outcome| api::QueueOperationOutcomeDto {
                operation_id: outcome.operation_id,
                applied: outcome.applied,
                reason: outcome.reason,
            },
        ),
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

/// 被控端收到一条 `Play`:按标识把执行副本取下来,**取全了**再换上。
///
/// 三条规矩都在这一段里(`docs/adr/0031` 七):
///
/// - **取失败保留旧副本,不执行半份列表。** 半份拿去放,用户听到的是一个他
///   没点过的队列;而旧副本至少还是他上一次点的那个。
/// - **换上是原子的**:曲目、队列、条目号三样一起换。中间空一拍的话,
///   那一拍里的自动续播会去读一个刚被清空的队列。
/// - **下场要回报**:成没成都记一笔,搭下一条报告捎给服务端。谎报已应用的话,
///   遥控器会把「新版本待应用」那个标记撤掉,而音箱里还是上一批。
///
/// 先把 `desired` 记下再去取:取的这几秒里,遥控器那头看到的应该是
/// 「新版本待应用」,而不是「什么都没发生」。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn adopt_remote_queue(
    ui: &MainWindow,
    deck: &Deck,
    queue_id: i64,
    revision: i64,
    entry_id: i64,
    operation_id: String,
) {
    log::info!(
        "收到执行副本请求: 队列 {queue_id}@{revision}, 条目 {entry_id}, \
         操作 {operation_id}"
    );

    let deck = deck.clone();
    let weak = ui.as_weak();
    let _ = slint::spawn_local(async move {
        // 三条闸与账本都在 `adopt_with` 里,这一段只管它交回来的下场
        // 落到屏幕上是什么样。
        let settled = adopt_with(
            &deck.execution,
            queue_id,
            revision,
            entry_id,
            operation_id,
            || deck.remote.is_controlled(),
            || api::fetch_queue(queue_id, revision),
        )
        .await;

        let Some(ui) = weak.upgrade() else { return };
        match settled {
            Adoption::AlreadyApplied => {
                log::info!(
                    "这一次点播已经应用过,不再重置播放"
                );
            }
            Adoption::Superseded => {
                log::info!("被更新的一次顶掉了,丢掉这一份");
            }
            Adoption::Dropped => {
                log::info!(
                    "取数期间本机已不再被遥控,丢掉这一次"
                );
            }
            Adoption::Failed(reason) => {
                log::warn!(
                    "取执行副本失败,保留旧的那一份: {reason}"
                );
                crate::notice::show(
                    &ui,
                    format!("队列没取下来: {reason}"),
                );
                // 下场要报出去,否则服务端那条意图永远挂在 pending 上,
                // 而遥控器会一直显示「新版本待应用」。
                checkpoint(
                    &deck,
                    deck.queue.borrow().index(),
                );
            }
            Adoption::Missing => {
                crate::notice::show(
                    &ui,
                    "要播的那一条不在这一版队列里"
                        .to_owned(),
                );
                checkpoint(
                    &deck,
                    deck.queue.borrow().index(),
                );
            }
            Adoption::Adopt { index, tracks } => {
                play_batch(&ui, &deck, tracks, index);
                // 换批之后立刻留一个检查点:遥控器那头正等着「待应用」那个
                // 标记消失,而它读的是服务端记下的 applied_revision。
                checkpoint(&deck, index);
                mark_sync(&ui, &deck);
            }
        }
    });
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
            adopt_remote_queue(
                ui,
                deck,
                queue_id,
                revision,
                entry_id,
                operation_id,
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
