//! 一条播放意图从按下去到落地的那一段(见 `music::playback::dispatch`)。
//!
//! 这一组走独奏(不在组里)那条路;组里的点歌与控制见 `tests/group.rs`(#142)。
//!
//! 入口一律用 Slint 的回调(`invoke_play` / `invoke_next_track` / …),不直接调
//! 内部函数:那两道闸从前就是写在回调入口上的,绕过入口去测,测的就不是
//! 用户真正走的那条路。

use similar_asserts::assert_eq;

use super::super::fixtures::*;
use super::super::*;
use crate::Player;

/// 把传输控件真正接到窗口上。
///
/// `deck_window` 只造一副 `Deck`,不接回调 —— 邻居那些测试一律直接调内部
/// 函数,所以从来不需要。而这一组要测的恰恰是**用户按下去**那条路:
/// 两道闸从前就写在回调入口上,绕过入口去调内部函数,测的就不是同一件事。
fn wire_transport(ui: &MainWindow, deck: &Deck) {
    bind_play(ui, deck);
    bind_controls(ui, deck);
    bind_volume(ui, deck);
    bind_seek(ui, deck);
}

/// 一批歌摆进「界面上那个列表」,也就是点歌时的批次来源。
fn batch_of(deck: &Deck, ids: &[&str]) -> Vec<TrackDto> {
    let batch: Vec<TrackDto> =
        ids.iter().map(|id| track_with_id(id)).collect();
    *deck.tracks.borrow_mut() = batch.clone();
    batch
}

/// 把本机的 playback 按在「这一首正在加载」上。
///
/// 没有直接的 setter —— `Playback::begin` 是私有的,而 `Loading` 恰好是
/// `app_core::play` 在第一个 await 之前留下的那一格。所以走真正那条路,
/// 喂它一个永远不就绪的 prepare:状态于是停在 Loading 上不动。
fn stall_on_loading(deck: &Deck, track: TrackDto) {
    let playback = deck.playback.clone();
    slint::spawn_local(async move {
        app_core::play(
            &playback,
            track,
            |_| async {
                std::future::pending::<Result<(), String>>()
                    .await
            },
            |()| {},
        )
        .await;
    })
    .expect("event loop must be running");
}

/// 输出在本机时,同一下走本机播放器,一条命令也不发。
#[test]
fn tapping_a_track_locally_starts_the_local_transport() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    batch_of(&deck, &["a", "b", "c"]);

    ui.global::<Player>().invoke_play("b".into());

    assert_eq!(
        deck.queue.borrow().current().map(|t| t.id.clone()),
        Some("b".to_owned()),
        "本机该从点中的那一首开始放"
    );
    assert_eq!(
        deck.queue.borrow().tracks().len(),
        3,
        "整批都该进队列,后面那些歌要能自动接上"
    );
    assert!(
        deck.group.intents().is_empty(),
        "独奏时不该发组意图"
    );
}

// ── 五处本机行为差异,各一条 ──

/// 差异 1:本机播放器放空之后按播放是**重播**,不是 resume 一个空播放器。
///
/// 队列放完之后播放器里没有源了,对着它 resume 什么也不会发生 —— 而界面上
/// 明明还写着一首歌的名字。遥控那侧没有这一档:上报里没有「播放器空没空」这一位。
#[test]
fn a_toggle_on_a_drained_player_replays_instead_of_resuming()
 {
    assert_eq!(
        local_toggle(false, true),
        LocalToggle::Replay,
        "放空了又按播放,该把当前这首从头放一遍"
    );
    assert_eq!(
        local_toggle(false, false),
        LocalToggle::Resume,
        "只是暂停着,接着放就行"
    );
    assert_eq!(
        local_toggle(true, false),
        LocalToggle::Pause
    );
}

/// 差异 2:跳转当场挂上「缓冲中」,不等那趟每秒的轮询。
///
/// 等轮询要慢一秒,而一秒的沉默正好是「点了没反应」。这一条现在归共享执行体,
/// 于是被控端替遥控器执行的那次跳转也会亮起缓冲 —— 从前那条路把 `seek` 的
/// 结果整个丢掉了。
#[test]
fn a_seek_marks_buffering_right_away() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    ui.global::<Player>().set_buffering(false);

    execute(&ui, &deck, Command::Seek { ms: 1_000 });

    assert!(
        ui.global::<Player>().get_buffering(),
        "跳转是异步的,那一刻就得让人看见它在动"
    );
}

/// 差异 3:音量存盘归**设备执行**这一侧。
///
/// 判据是那句「音量跟着设备走,不跟着账号」:真正改变响度的是这台机器的
/// 播放器,那么记住这个数的也该是这台机器 —— 不管拧旋钮的手是本机用户的,
/// 还是遥控器的。从前只有本机那条路存,于是遥控器把被控端调小之后,
/// 被控端一重启就跳回原来的音量。
#[test]
fn executing_a_volume_command_remembers_it_for_this_device()
{
    let _file = SETTINGS_FILE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);

    execute(&ui, &deck, Command::Volume { level: 0.25 });
    // 存盘是节流的(#137 ⑥):拖完停一下才写。原断言不变,只是等它落盘
    settle_volume_save();

    assert_eq!(
        api::settings::load().volume,
        0.25,
        "执行音量的那一端该把它记住"
    );
    assert_eq!(ui.global::<Player>().get_volume(), 0.25);
}

/// 设置文件是进程级的一份,测试并行跑。会真写它的测试(让节流存盘到点的那几条)
/// 先拿这把锁,否则一条断言到的是另一条刚写进去的数。
static SETTINGS_FILE: std::sync::Mutex<()> =
    std::sync::Mutex::new(());

/// 让音量的节流存盘到点。
fn settle_volume_save() {
    i_slint_backend_testing::mock_elapsed_time(
        VOLUME_SAVE_DELAY
            + core::time::Duration::from_millis(50),
    );
    slint::platform::update_timers_and_animations();
}

/// 拖音量滑块是一串连着的命令:每动一下都同步读写一次设置文件,UI 线程上
/// 就是每帧一次磁盘 IO(#137 ⑥)。停手之后只写一次,写的是最后那个值。
#[test]
fn a_volume_drag_is_saved_once_it_settles() {
    let _file = SETTINGS_FILE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);

    for level in [0.31, 0.32, 0.33] {
        execute(&ui, &deck, Command::Volume { level });
    }

    assert_ne!(
        api::settings::load().volume,
        0.33,
        "还在拖的时候不该每动一下就写盘"
    );
    assert_eq!(
        ui.global::<Player>().get_volume(),
        0.33,
        "界面与播放器照样当场跟手"
    );

    settle_volume_save();
    assert_eq!(
        api::settings::load().volume,
        0.33,
        "停手之后写的是最后那个值"
    );
}

/// 同一条命令里,超出 0..=1 的音量**先夹再落**。
///
/// 不夹的话,一个发疯的遥控器能把本机音量设成 8 倍 —— 那一下是听得见的。
#[test]
fn a_volume_command_is_clamped_before_it_lands() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);

    execute(&ui, &deck, Command::Volume { level: 1.5 });

    assert_eq!(ui.global::<Player>().get_volume(), 1.0);
}

/// 本机上同一下仍然要被去重拦住:连点五下就是五条在途下载,
/// 每条回来都往播放器里塞一次源,声音从头响五遍。
#[test]
fn a_second_tap_on_a_loading_track_is_dropped_locally() {
    let (ui, deck) = deck_window_pumped();
    wire_transport(&ui, &deck);
    batch_of(&deck, &["a", "b"]);
    stall_on_loading(&deck, track_with_id("b"));

    ui.global::<Player>().invoke_play("b".into());

    assert!(
        deck.queue.borrow().current().is_none(),
        "同一首已经在加载了,这一下是多余的"
    );
}

/// 输出在本机时,「下一首」落到本机队列上。
#[test]
fn next_track_advances_the_local_queue() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    let batch = batch_of(&deck, &["a", "b"]);
    deck.queue.borrow_mut().replace(batch, 0);

    ui.global::<Player>().invoke_next_track();

    assert_eq!(
        deck.queue.borrow().current().map(|t| t.id.clone()),
        Some("b".to_owned()),
        "切歌要落到本机队列上"
    );
}

/// **本机点一次歌,只发布一次队列**(#125)。
///
/// 发布还在路上时 `queue_id` 是空的,每秒那一趟 tick 问「要不要补同步」,
/// 补同步的钟又从没走过,于是紧跟着再 `POST /queues` 一次 —— 真机上看到的
/// 就是间隔一两百毫秒的两条。
#[test]
fn tapping_a_track_locally_publishes_the_queue_once() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    batch_of(&deck, &["a", "b", "c"]);

    ui.global::<Player>().invoke_play("b".into());
    // 发布的回包还没到,每秒那一趟先来了。
    resync_local_queue(&ui, &deck);

    assert_eq!(
        deck.execution.publishes(),
        1,
        "点一次歌只该发布一次队列"
    );
}

// ── 连点同一首(#125 范围扩展)──
//
// 第一下到出声真机上要 1.7~2 秒,这期间用户本能地再点一下。同一首还在
// 加载或已经在放,再点就忽略:不打断、不重来、不再发布一次队列。加载中
// 那一种早有 `a_second_tap_on_a_loading_track_is_dropped_locally`。

/// 让本机的播放状态停在「`id` 正在加载」:准备那一步永远不回来。
fn hold_loading(deck: &Deck, id: &str) {
    poll_once(app_core::play(
        &deck.playback,
        track_with_id(id),
        |_| std::future::pending::<Result<(), String>>(),
        |_| {},
    ));
}

fn poll_once(
    future: impl core::future::Future<Output = ()>,
) {
    let mut future = core::pin::pin!(future);
    let _ = future.as_mut().poll(
        &mut core::task::Context::from_waker(
            core::task::Waker::noop(),
        ),
    );
}

/// 用户点过 `id`、它成了本机队列的当前一首。
fn tapped_before(deck: &Deck, id: &str) {
    let batch = deck.tracks.borrow().clone();
    let index = batch
        .iter()
        .position(|track| track.id == id)
        .expect("点的那首在这一批里");
    deck.queue.borrow_mut().replace(batch, index);
}

/// 还在加载的那一首再点一下:经回调入口走到去重那道闸,被挡下 —— 不发布、
/// 不重新加载。
///
/// 这一条原先测的是「已在**响**的那一首」,拿界面上的 `is-playing` 当「在响」
/// 的输入。#137 ③ 起播放逻辑不回读界面属性(界面是投影),「在响」问播放器
/// 本身,而测试里没有声卡 —— 那个前提在这里造不出来。「在响的那首再点是多余的」
/// 这条规则本身由 `rules::tests::tapping_the_sounding_track_is_redundant` 钉着;
/// 这里改用加载中那一档,验的仍是**回调入口确实经过了去重那道闸**。
#[test]
fn tapping_the_loading_track_again_is_ignored() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    batch_of(&deck, &["a", "b", "c"]);
    tapped_before(&deck, "b");
    poll_once(app_core::play(
        &deck.playback,
        track_with_id("b"),
        |_| core::future::pending::<Result<(), String>>(),
        |()| {},
    ));

    ui.global::<Player>().invoke_play("b".into());

    assert_eq!(
        deck.execution.publishes(),
        0,
        "加载中的那一首再点,不该再发布一次队列"
    );
    assert!(
        matches!(
            deck.playback.borrow().state(),
            PlaybackState::Loading(track) if track.id == "b"
        ),
        "加载中的那一首不该被停下来从头再加载"
    );
}

#[test]
fn tapping_another_track_while_loading_switches() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    batch_of(&deck, &["a", "b", "c"]);
    tapped_before(&deck, "b");
    hold_loading(&deck, "b");

    ui.global::<Player>().invoke_play("c".into());

    assert_eq!(
        deck.queue.borrow().current().map(|t| t.id.clone()),
        Some("c".to_owned()),
        "点的是另一首就照常切过去"
    );
    assert_eq!(deck.execution.publishes(), 1);
}
