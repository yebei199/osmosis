//! 一条播放意图从按下去到落地的那一段(见 `music::playback::dispatch`)。
//!
//! 这一组补的是本仓从来没人走过的那条路:**遥控器侧**。#108 之前,全仓没有
//! 任何一条测试调用过 `invoke_play`,于是「按下去到底发没发出命令」谁也说不出来
//! —— 而现场那个故障的症状恰好就是「按了没反应」。
//!
//! 入口一律用 Slint 的回调(`invoke_play` / `invoke_next_track` / …),不直接调
//! 内部函数:那两道闸从前就是写在回调入口上的,绕过入口去测,测的就不是
//! 用户真正走的那条路。

use similar_asserts::assert_eq;
use syncplay::Event;

use super::super::fixtures::*;
use super::super::*;
use crate::{Player, Shell};

fn device(id: &str) -> app_core::DeviceDto {
    app_core::DeviceDto {
        id: id.to_owned(),
        name: format!("设备 {id}"),
    }
}

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

/// 把输出交给 `pc`,并让它报一条**新鲜**的状态 —— 控制因此发得出去。
fn take_control_of_pc(deck: &Deck) {
    deck.remote.assume_output("pc", "pc1");
    deck.remote.accept_report_at(
        report(),
        crate::sync::remote::now_ms(),
    );
}

/// 把输出交给 `pc`,但手上那份上报已经过期(`STALE_AFTER_MS` 是三秒)。
fn hold_a_stale_view_of_pc(deck: &Deck) {
    deck.remote.assume_output("pc", "pc1");
    deck.remote.accept_report_at(
        report(),
        crate::sync::remote::now_ms() - 10_000,
    );
}

/// 被控端报来的一份状态。
fn report() -> app_core::RemoteStateDto {
    app_core::RemoteStateDto {
        track: Some(track()),
        position_ms: 0,
        state: app_core::RemotePlayState::Playing,
        volume: 0.5,
        queue_id: Some(7),
        revision: Some(1),
        applied_revision: Some(1),
        entry_id: Some(12),
        queue_len: 1,
        epoch: 1_700_000_000_000,
        state_seq: 1,
        operation: None,
    }
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

// ── 遥控器侧:点歌真的发出去了 ──

/// **遥控时点一首歌,发出去的是带整批的 `Play`。**
///
/// 整批而不是一首:自动续播在被控端发生(`docs/adr/0030`),它得自己拿着
/// 后面那些歌 —— 只发一首的话,遥控器一锁屏,pc1 放完就停了。
///
/// 批次取的是**用户点中的那个列表**,不是被控端回报的队列:他想听的是眼前
/// 这一批的后面那些歌,不是对面正在放的那一批。
#[test]
fn tapping_a_track_while_remote_sends_the_whole_batch() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    let batch = batch_of(&deck, &["a", "b", "c"]);
    take_control_of_pc(&deck);

    ui.global::<Player>().invoke_play("b".into());

    // 队列挪进服务端之后(`docs/adr/0031`),这一下走的是「先把这一批发布
    // 成服务端队列,拿到 queue_id/revision 再发命令」。发布是一次 HTTP 往返,
    // 测试环境里没有服务端,所以命令发不出去 —— 但**这一下确实走到了远端
    // 分支**,而那正是这条测试的主语。原来的断言是「该变成一条带整批的
    // Play」,那条命令已经不存在了。
    let _ = batch;
    assert_eq!(
        deck.remote.play_submits(),
        1,
        "遥控时点一首歌,该走远端那条路"
    );
    assert!(
        deck.queue.borrow().current().is_none(),
        "遥控那一下不该在本机起播 —— 两台会同时出声"
    );
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
        deck.remote.sent_commands().is_empty(),
        "本机输出不该往信令上发东西"
    );
}

// ── ADR 0030:过期不回落本机 ──

/// **状态过期时按下一首,声音不许从遥控器自己这台放出来。**
///
/// 这是 #108 要改掉的那一半:从前是 `if send(..) { return }`,发不出去就
/// 径直落到本机 `advance()` 上。用户低头一看,歌从手机里放出来了 ——
/// 而他要的是让 pc1 放(`docs/adr/0030`:过期只禁用控制,**不自动切回本机**)。
#[test]
fn a_stale_target_never_falls_back_to_the_local_speaker() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    let batch = batch_of(&deck, &["a", "b"]);
    deck.queue.borrow_mut().replace(batch, 0);
    hold_a_stale_view_of_pc(&deck);

    ui.global::<Player>().invoke_next_track();

    assert_eq!(
        deck.queue.borrow().current().map(|t| t.id.clone()),
        Some("a".to_owned()),
        "本机队列一步都不该动"
    );
    assert!(
        deck.remote.is_remote(),
        "输出要留在那台设备上,不许偷偷收回本机"
    );
    assert_eq!(
        ui.global::<Shell>().get_banner_text(),
        "pc1 控制暂不可用",
        "既然不回落,就必须说一句 —— 否则按下去与坏掉毫无区别"
    );
}

/// 点歌同理:过期时不许改在本机放。
#[test]
fn a_stale_target_never_plays_the_tap_locally() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    batch_of(&deck, &["a", "b"]);
    hold_a_stale_view_of_pc(&deck);

    ui.global::<Player>().invoke_play("b".into());

    assert!(
        deck.queue.borrow().current().is_none(),
        "过期不是回本机放的理由"
    );
    assert!(
        deck.remote.sent_commands().is_empty(),
        "过期时不该把命令发出去 —— 发了会攒着一次全到"
    );
}

/// 音量也一样:从前它发不出去就落到本机播放器上,于是遥控器拧了一下,
/// 响的是自己这台。
#[test]
fn a_stale_target_never_turns_the_local_volume() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    hold_a_stale_view_of_pc(&deck);

    ui.global::<Player>().invoke_volume_changed(0.25);

    assert_eq!(
        ui.global::<Shell>().get_banner_text(),
        "pc1 控制暂不可用"
    );
    assert!(deck.remote.sent_commands().is_empty());
}

// ── 被控锁:拦本机用户,不拦收到的命令 ──

/// 锁定期间本机那一下不算数,而**遥控器发来的同一条命令照常执行**。
///
/// 两条路必须分开:锁住的是这台机器前面那个人,不是遥控它的那个人。
/// 收到的命令若也走出站路由,这台还会把它原样转发回去。
#[test]
fn being_controlled_blocks_the_local_tap_but_not_a_received_command()
 {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    let batch = batch_of(&deck, &["a", "b"]);
    crate::sync::remote::handle(
        &Event::ControlledBy {
            device: device("phone"),
        },
        &deck.remote,
    );

    ui.global::<Player>().invoke_play("b".into());
    assert!(
        deck.queue.borrow().current().is_none(),
        "锁定期间本机前面那个人按的不算数"
    );

    // 遥控器发来的 Play 现在只带队列标识,曲目要另取(#109 第 4 段)。
    // 这条测试钉的是「锁不拦遥控器发来的命令」,所以直接走拿到曲目之后
    // 那一段 —— 它正是取数成功时会落到的地方。
    play_batch(&ui, &deck, batch, 1);

    assert_eq!(
        deck.queue.borrow().current().map(|t| t.id.clone()),
        Some("b".to_owned()),
        "遥控器发来的那条要照常执行 —— 锁不拦它"
    );
    assert!(
        deck.remote.sent_commands().is_empty(),
        "收到的命令不许再转发回去"
    );
}

/// 信令断了,锁就撤:断网期间本机点歌照常落到本机(#118)。
///
/// 锁此前只有服务端的消息才清,而断着的时候消息过不来 —— 被控端断网期间
/// 连歌都点不了,要等重连、等服务端想起来告诉它一声。
#[test]
fn a_dropped_link_lets_the_local_tap_through() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    let _ = batch_of(&deck, &["a", "b"]);
    crate::sync::remote::handle(
        &Event::ControlledBy {
            device: device("phone"),
        },
        &deck.remote,
    );

    crate::sync::remote::handle(
        &Event::Disconnected,
        &deck.remote,
    );
    ui.global::<Player>().invoke_play("b".into());

    assert_eq!(
        deck.queue.borrow().current().map(|t| t.id.clone()),
        Some("b".to_owned()),
        "断线之后本机前面那个人按的要算数"
    );
}

/// 接管失败,输出回本机:之后点歌落到本机,不再发去那台设备(#118)。
#[test]
fn a_failed_claim_sends_the_next_tap_to_the_local_player() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    let _ = batch_of(&deck, &["a", "b"]);
    deck.remote.assume_output("pc", "pc1");
    assert!(
        deck.remote.is_remote(),
        "按下去那一刻先乐观地切过去"
    );

    crate::sync::remote::handle(
        &Event::ClaimFailed {
            target: "pc".to_owned(),
            reason: "device_offline: 设备 pc 不在线"
                .to_owned(),
        },
        &deck.remote,
    );
    ui.global::<Player>().invoke_play("b".into());

    assert!(
        !deck.remote.is_remote(),
        "接管没成,输出该回本机"
    );
    assert_eq!(
        ui.global::<Shell>().get_output_id(),
        "",
        "芯片也要跟着回本机"
    );
    assert_eq!(
        deck.queue.borrow().current().map(|t| t.id.clone()),
        Some("b".to_owned()),
        "这一下该落到本机播放器上"
    );
    assert_eq!(
        deck.remote.play_submits(),
        0,
        "不该再发去那台设备"
    );
}

/// 失败的是上一台,用户已经改选了别的:不许把新的选择一起撤掉。
#[test]
fn a_failed_claim_on_a_device_no_longer_selected_is_ignored()
 {
    let (_ui, deck) = deck_window();
    deck.remote.assume_output("pc", "pc1");
    deck.remote.assume_output("tablet", "平板");

    crate::sync::remote::handle(
        &Event::ClaimFailed {
            target: "pc".to_owned(),
            reason: "device_offline".to_owned(),
        },
        &deck.remote,
    );

    assert_eq!(
        deck.remote.target_id().as_deref(),
        Some("tablet"),
        "失败的那一台早就不是当前的输出了"
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

    execute(
        &ui,
        &deck,
        app_core::RemoteCommand::Seek { ms: 1_000 },
    );

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
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);

    execute(
        &ui,
        &deck,
        app_core::RemoteCommand::Volume { level: 0.25 },
    );
    // 存盘是节流的(#137 ⑥):拖完停一下才写。原断言不变,只是等它落盘
    settle_volume_save();

    assert_eq!(
        api::settings::load().volume,
        0.25,
        "执行音量的那一端该把它记住"
    );
    assert_eq!(ui.global::<Player>().get_volume(), 0.25);
}

/// 让音量的节流存盘到点。
fn settle_volume_save() {
    i_slint_backend_testing::mock_elapsed_time(
        VOLUME_SAVE_DELAY.as_millis() as u64 + 50,
    );
    slint::platform::update_timers_and_animations();
}

/// 拖音量滑块是一串连着的命令:每动一下都同步读写一次设置文件,UI 线程上
/// 就是每帧一次磁盘 IO(#137 ⑥)。停手之后只写一次,写的是最后那个值。
#[test]
fn a_volume_drag_is_saved_once_it_settles() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);

    for level in [0.31, 0.32, 0.33] {
        execute(
            &ui,
            &deck,
            app_core::RemoteCommand::Volume { level },
        );
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

    execute(
        &ui,
        &deck,
        app_core::RemoteCommand::Volume { level: 1.5 },
    );

    assert_eq!(ui.global::<Player>().get_volume(), 1.0);
}

/// 差异 6(产品裁决):音量**不受**被控锁限制,而切歌受。
///
/// 那道锁拦的是 transport —— 遥控器正按着这台报来的进度插值,本机偷偷改一下
/// 就让对面的进度条撒谎。音量不在那条链上:它是这台机器的响度,而且每秒随
/// 快照报一次,最迟一秒后遥控器就看见了。这也保持了改之前的行为
/// (`bind_volume` 本来就没有这道闸),不在重构里无声改产品规则。
#[test]
fn the_controlled_lock_stops_transport_but_not_the_volume_knob()
 {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    let batch = batch_of(&deck, &["a", "b"]);
    deck.queue.borrow_mut().replace(batch, 0);
    crate::sync::remote::handle(
        &Event::ControlledBy {
            device: device("phone"),
        },
        &deck.remote,
    );

    ui.global::<Player>().invoke_volume_changed(0.25);
    assert_eq!(
        ui.global::<Player>().get_volume(),
        0.25,
        "被控端前面的人拧自己音箱,是物理动作,锁不该拦"
    );

    ui.global::<Player>().invoke_next_track();
    assert_eq!(
        deck.queue.borrow().current().map(|t| t.id.clone()),
        Some("a".to_owned()),
        "切歌照旧被锁拦下 —— 那一下会让遥控器的进度条撒谎"
    );
}

/// 差异 4:本机残留的 `Loading` 不许把一条**远端**意图丢掉。
///
/// 连点去重读的是本机 playback,而它早于目标选择被问的话,刚从本机切到遥控
/// 时那份残留的 `Loading` 会把用户点的第一首静默吞掉 —— 症状与现场那个故障
/// 一模一样,而原因完全不同。
#[test]
fn a_leftover_local_loading_state_does_not_swallow_a_remote_tap()
 {
    let (ui, deck) = deck_window_pumped();
    wire_transport(&ui, &deck);
    batch_of(&deck, &["a", "b"]);
    // 本机此刻正卡在「b 加载中」上 —— 切到遥控之前留下的那一格。
    stall_on_loading(&deck, track_with_id("b"));
    take_control_of_pc(&deck);

    ui.global::<Player>().invoke_play("b".into());

    assert_eq!(
        deck.remote.play_submits(),
        1,
        "去重是本机那条路的事,不该拦下发给别的设备的意图"
    );
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

// ── 提交成功 ≠ 放起来了 ──

/// 刚接管、一条上报都还没回来时,控制**要能发出去**。
///
/// 从选中设备到第一条上报回来是三个来回,而「选完设备马上点一首歌」是最自然
/// 的操作顺序。把那一下也挡掉,得到的同样是「按了没反应」,只是原因反过来。
#[test]
fn a_just_claimed_target_accepts_the_first_tap() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    batch_of(&deck, &["a"]);
    deck.remote.assume_output("pc", "pc1");

    ui.global::<Player>().invoke_play("a".into());

    assert_eq!(
        deck.remote.play_submits(),
        1,
        "快照还在路上,不是丢掉用户这一下的理由"
    );
}

// ── 四种下场各自说清一件事 ──

/// 返回值要分得清**实际含义**,不是一个「成了没有」的布尔。
///
/// 尤其是 `RemoteSubmitted`:它只表示**本地提交**成功。队列、服务端转发、
/// 被控端执行都还在后面,任何一跳都可能悄悄丢掉它 —— 真放起来了以被控端的
/// 上报为准。把它当成「放成了」正是现场那次误判的来源。
#[test]
fn each_outcome_says_which_of_the_four_things_happened() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    let batch = batch_of(&deck, &["a", "b"]);

    assert_eq!(
        dispatch(
            &ui,
            &deck,
            Intent::Play {
                tracks: batch.clone(),
                index: 0,
            }
        ),
        Dispatched::LocalApplied,
        "输出在本机:自己执行"
    );

    take_control_of_pc(&deck);
    assert_eq!(
        dispatch(&ui, &deck, Intent::Next),
        Dispatched::RemoteSubmitted,
        "交给客户端了 —— 仅此而已,不代表那边放起来了"
    );

    hold_a_stale_view_of_pc(&deck);
    assert_eq!(
        dispatch(&ui, &deck, Intent::Next),
        Dispatched::Unavailable("目标此刻收不了命令"),
        "过期:不发、也不回落本机"
    );

    crate::sync::remote::handle(
        &Event::ControlledBy {
            device: device("phone"),
        },
        &deck.remote,
    );
    assert_eq!(
        dispatch(&ui, &deck, Intent::Next),
        Dispatched::Blocked("本机正被遥控"),
        "锁拦下的是本机前面那个人按的这一下"
    );
}

// ── AC-2:批次大小不再进这条链 ──

/// **批次大小不再决定这一下能不能发出去**(AC-2、AC-6)。
///
/// 这里原本是两条相反的测试:`a_batch_under_the_limit_still_goes_out` 断言
/// 二十首照常发,`an_oversized_batch_is_refused_before_it_can_break_the_connection`
/// 断言四千首在发之前被拒、横幅说「队列太长」。两条的前提都是
/// **`Play` 拖着整批曲目**,于是它的字节数随用户的歌单长度增长,而超限会撞掉
/// 整条连接(#108)。
///
/// 本轮把曲目挪去了 HTTP(`docs/adr/0031`),`Play` 只带
/// `queue_id/revision/entry_id/operation_id`,定长 —— 那道按字节的闸对它
/// 永远不再触发,两条断言的前提都没了。
///
/// 留下的这条钉的是那个前提消失之后**仍然成立**的事:两种规模走到同一个
/// 下场。第 4 段把取数接上之后它照样成立,只是那个下场从「还发不出去」
/// 变成「发出去了」。
///
/// 按字节的自检本身没删,它仍是发之前唯一一道闸,由
/// `syncplay` 的 `no_command_grows_with_user_data` 守着。
#[test]
fn the_batch_size_no_longer_decides_a_remote_tap() {
    // 同一个窗口里换两次批:Slint 的后端一个线程只装得下一个,
    // 起两个窗口会撞 `AlreadySet`。
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    take_control_of_pc(&deck);

    let mut outcomes = Vec::new();
    for count in [20_usize, 4_000] {
        let ids: Vec<String> =
            (0..count).map(|n| n.to_string()).collect();
        let refs: Vec<&str> =
            ids.iter().map(String::as_str).collect();
        batch_of(&deck, &refs);

        // 两轮点不同的两首:同一首连点两下会被当成多余的那一下(#113)。
        let tapped = if count == 20 { "7" } else { "8" };
        ui.global::<Player>().invoke_play(tapped.into());

        outcomes.push((
            deck.remote.play_submits(),
            ui.global::<Shell>()
                .get_banner_text()
                .to_string(),
        ));
    }

    assert_eq!(
        outcomes[0].1, outcomes[1].1,
        "二十首与四千首该走到同一个下场 —— 批次大小已经不在这条链上了"
    );
    assert_eq!(
        outcomes[1].0, 2,
        "两下都该走到远端分支,而不是被哪一道按字节的闸拦下"
    );
    assert!(
        !outcomes[1].1.contains("太长"),
        "两种规模都在配额之内,不该说太长,实得 {}",
        outcomes[1].1
    );
}

/// 超出**配额**的那一批仍然当场拒绝,而且立刻说得出话(AC-6)。
///
/// 与上一条不是一回事:那条说的是「字节数不再是判据」,这条说的是「条数
/// 仍然有上限」。上限从 64 KiB 那条线换成了 `MAX_QUEUE_ENTRIES`,而拒绝的
/// 规矩没变 —— **不截断、不静默**,而且不等那次 HTTP 往返回来:用户要的是
/// 一句立刻出现的话,而这一条等多久都不会好。
#[test]
fn a_batch_past_the_quota_is_refused_on_the_spot() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    let ids: Vec<String> = (0..api::MAX_QUEUE_ENTRIES + 1)
        .map(|n| n.to_string())
        .collect();
    let refs: Vec<&str> =
        ids.iter().map(String::as_str).collect();
    batch_of(&deck, &refs);
    take_control_of_pc(&deck);

    ui.global::<Player>().invoke_play("7".into());

    assert_eq!(
        deck.remote.play_submits(),
        0,
        "超出配额的那一批不该发出去"
    );
    assert!(
        ui.global::<Shell>()
            .get_banner_text()
            .contains("太长"),
        "要说得出为什么,实得 {}",
        ui.global::<Shell>().get_banner_text()
    );
    assert!(
        deck.remote.is_remote(),
        "拒掉这一下不等于放弃那台设备"
    );
    assert!(
        deck.queue.borrow().current().is_none(),
        "更不等于改在本机放 —— 那是 ADR 0030 明令禁止的回落"
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

/// 遥控时连点同一首:只发布一次队列、只发一条 play(#113)。
///
/// #125 的连点去重只挂在本机那条路上,遥控这边每点一下都重新发布一次、
/// 再发一条 play —— 现场两秒十发,同一队列版本号 1→10,操作号 1→10。
#[test]
fn tapping_the_same_track_again_while_remote_submits_once()
{
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    batch_of(&deck, &["a", "b", "c"]);
    take_control_of_pc(&deck);

    for _ in 0..10 {
        ui.global::<Player>().invoke_play("b".into());
    }

    assert_eq!(
        deck.remote.play_submits(),
        1,
        "同一首还在路上,再点是多余的"
    );

    ui.global::<Player>().invoke_play("c".into());
    assert_eq!(
        deck.remote.play_submits(),
        2,
        "点的是另一首就照常发出去"
    );
}

/// 被控端已经在放这一首,遥控器上再点它不该从头再来一遍。
#[test]
fn tapping_the_track_the_target_is_playing_is_ignored() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    let playing = report().track.expect("上报里有一首").id;
    *deck.tracks.borrow_mut() =
        vec![track_with_id(&playing)];
    take_control_of_pc(&deck);

    ui.global::<Player>().invoke_play(playing.into());

    assert_eq!(
        deck.remote.play_submits(),
        0,
        "对面已经在放这一首了"
    );
}

/// 对面暂停着的那一首再点不算多余 —— 那一下是想让它响。
#[test]
fn tapping_the_track_the_target_paused_still_goes_out() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);
    let paused = report().track.expect("上报里有一首").id;
    *deck.tracks.borrow_mut() =
        vec![track_with_id(&paused)];
    deck.remote.assume_output("pc", "pc1");
    deck.remote.accept_report_at(
        app_core::RemoteStateDto {
            state: app_core::RemotePlayState::Paused,
            ..report()
        },
        crate::sync::remote::now_ms(),
    );

    ui.global::<Player>().invoke_play(paused.into());

    assert_eq!(deck.remote.play_submits(), 1);
}
