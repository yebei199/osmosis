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
    deck.remote.select("pc", "pc1");
    deck.remote.accept_report_at(
        report(),
        crate::sync::remote::now_ms(),
    );
}

/// 把输出交给 `pc`,但手上那份上报已经过期(`STALE_AFTER_MS` 是三秒)。
fn hold_a_stale_view_of_pc(deck: &Deck) {
    deck.remote.select("pc", "pc1");
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
        queue: vec![track()],
        queue_index: 0,
        volume: 0.5,
        sent_at: 1,
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

    assert_eq!(
        deck.remote.sent_commands(),
        vec![app_core::RemoteCommand::Play {
            tracks: batch,
            index: 1,
        }],
        "遥控时这一下该变成一条带整批的 Play"
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

    execute(&ui, &deck, app_core::RemoteCommand::Play {
        tracks: batch,
        index: 1,
    });

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
    assert_eq!(local_toggle(true, false), LocalToggle::Pause);
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

    execute(&ui, &deck, app_core::RemoteCommand::Seek {
        ms: 1_000,
    });

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
fn executing_a_volume_command_remembers_it_for_this_device() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);

    execute(&ui, &deck, app_core::RemoteCommand::Volume {
        level: 0.25,
    });

    assert_eq!(
        api::settings::load().volume,
        0.25,
        "执行音量的那一端该把它记住"
    );
    assert_eq!(ui.global::<Player>().get_volume(), 0.25);
}

/// 同一条命令里,超出 0..=1 的音量**先夹再落**。
///
/// 不夹的话,一个发疯的遥控器能把本机音量设成 8 倍 —— 那一下是听得见的。
#[test]
fn a_volume_command_is_clamped_before_it_lands() {
    let (ui, deck) = deck_window();
    wire_transport(&ui, &deck);

    execute(&ui, &deck, app_core::RemoteCommand::Volume {
        level: 1.5,
    });

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

/// 差异 5:本机残留的 `Loading` 不许把一条**远端**意图丢掉。
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
        deck.remote.sent_commands().len(),
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

/// 差异 4:退出收听的规矩各命令不同,不能一把 `leave()` 盖全部。
///
/// 切歌退出后**继续**作用于本机队列 —— 点了「下一首」的人想听的是自己的
/// 下一首,不是单纯安静下来。⏯ 则退出即止。
#[test]
fn leaving_a_listening_session_still_advances_the_local_queue()
 {
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
    deck.remote.select("pc", "pc1");

    ui.global::<Player>().invoke_play("a".into());

    assert_eq!(
        deck.remote.sent_commands().len(),
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
        dispatch(&ui, &deck, Intent::Play {
            tracks: batch.clone(),
            index: 0,
        }),
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
        Dispatched::Unavailable,
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
