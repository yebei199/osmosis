//! 遥控两端的接线:被控端执行命令,以及遥控器侧的事件落地。
//!
//! 这两条路径都跑在「不是普通回调」的位置上 —— 命令绕过回调入口那两道闸
//! (锁住的是这台机器前面的人,不是遥控它的那个人),事件跑在后台线程上。
//! 正因为绕过了平常那条路,它们不测就真的没人走过。

use similar_asserts::assert_eq;
use syncplay::Event;

use super::super::fixtures::*;
use super::super::*;
use crate::Player;

fn device(id: &str) -> app_core::DeviceDto {
    app_core::DeviceDto {
        id: id.to_owned(),
        name: format!("设备 {id}"),
    }
}

fn report(
    position_ms: u64,
    state: app_core::RemotePlayState,
) -> app_core::RemoteStateDto {
    app_core::RemoteStateDto {
        track: Some(track()),
        position_ms,
        state,
        volume: 0.5,
        queue_id: Some(7),
        revision: Some(1),
        applied_revision: Some(1),
        entry_id: Some(12),
        queue_len: 1,
        epoch: 1_700_000_000_000,
        state_seq: position_ms,
        operation: None,
    }
}

// ── 被控端:遥控器发来的命令落到本机播放器上 ──

/// 暂停命令把本机的播放态按下去。
///
/// 这一条与控制条上那颗键走的**不是**同一条路:回调入口上有「被遥控时不生效」
/// 那道闸,而命令正是那道闸放行的唯一来源。绕过去是对的,所以也得单独验。
#[test]
fn a_remote_pause_command_stops_the_local_transport() {
    let (ui, deck) = deck_window();
    ui.global::<Player>().set_is_playing(true);

    execute(&ui, &deck, app_core::RemoteCommand::Pause);

    assert!(!ui.global::<Player>().get_is_playing());
}

/// 继续命令把它抬起来。
#[test]
fn a_remote_resume_command_starts_the_local_transport() {
    let (ui, deck) = deck_window();
    ui.global::<Player>().set_is_playing(false);

    execute(&ui, &deck, app_core::RemoteCommand::Resume);

    assert!(ui.global::<Player>().get_is_playing());
}

/// 音量命令落到滑块上,并且**先夹再落**。
///
/// 不夹的话,一个发疯的遥控器能把本机音量设成 8 倍 —— 而那一下是听得见的。
#[test]
fn a_remote_volume_command_is_clamped_before_it_lands() {
    let (ui, deck) = deck_window();

    execute(
        &ui,
        &deck,
        app_core::RemoteCommand::Volume { level: 1.5 },
    );

    assert_eq!(ui.global::<Player>().get_volume(), 1.0);
}

/// 播放命令把**整批**装进队列,并从指定那一首开始。
///
/// 整批而不是一首:自动续播在被控端发生,它得自己拿着后面那些歌 ——
/// 只收一首的话,遥控器一锁屏 pc1 放完就停了。
#[test]
fn a_remote_play_command_loads_the_whole_batch() {
    let (ui, deck) = deck_window();
    let batch = vec![
        track_with_id("a"),
        track_with_id("b"),
        track_with_id("c"),
    ];

    // 线上那条 `Play` 现在只带队列标识,曲目由被控端按 queue_id/revision
    // 自己取(`docs/adr/0031`)。取数那一段是 #109 第 4 段;这条测试钉的是
    // 「拿到整批之后,后面那些歌留在队列里、从第 index 首开始放」——
    // 那正是取数成功之后会落到的地方,也是自动续播得以在这一端发生的原因。
    play_batch(&ui, &deck, batch.clone(), 1);

    assert_eq!(
        deck.queue.borrow().tracks().len(),
        3,
        "后面那些歌也得留在队列里"
    );
    assert_eq!(
        deck.queue.borrow().current().map(|t| t.id.clone()),
        Some("b".to_owned()),
        "该从第 index 首开始"
    );
}

/// 下一首命令推进队列。
#[test]
fn a_remote_next_command_advances_the_queue() {
    let (ui, deck) = deck_window();
    play_batch(
        &ui,
        &deck,
        vec![track_with_id("a"), track_with_id("b")],
        0,
    );

    execute(&ui, &deck, app_core::RemoteCommand::Next);

    assert_eq!(
        deck.queue.borrow().current().map(|t| t.id.clone()),
        Some("b".to_owned())
    );
}

/// 上一首命令退回去。
#[test]
fn a_remote_prev_command_steps_back() {
    let (ui, deck) = deck_window();
    play_batch(
        &ui,
        &deck,
        vec![track_with_id("a"), track_with_id("b")],
        1,
    );

    execute(&ui, &deck, app_core::RemoteCommand::Prev);

    assert_eq!(
        deck.queue.borrow().current().map(|t| t.id.clone()),
        Some("a".to_owned())
    );
}

// ── 被控端:锁定态 ──

/// 被遥控期间,这台机器前面的人按播放键不算数(产品规则)。
///
/// 少了这道锁,pc1 前面的人随手按一下暂停,手机上的进度条就开始撒谎。
#[test]
fn being_controlled_locks_the_local_transport() {
    let (ui, deck) = deck_window();
    crate::sync::remote::handle(
        &Event::ControlledBy {
            device: device("phone"),
        },
        &deck.remote,
    );
    ui.global::<Player>().set_is_playing(true);

    dispatch(&ui, &deck, Intent::TogglePlay);

    assert!(deck.remote.is_controlled(), "该进锁定态");
    assert!(
        ui.global::<Player>().get_is_playing(),
        "锁定期间本机那一下不该改变播放态"
    );
}

/// 遥控器发来的命令进收件箱,等 UI 线程来取。
///
/// 事件跑在后台线程上,而执行要碰 Deck(全是 Rc)—— 中间这一格队列是必须的。
#[test]
fn a_command_from_the_controller_lands_in_the_inbox() {
    let (_ui, deck) = deck_window();

    crate::sync::remote::handle(
        &Event::Command {
            cmd: app_core::RemoteCommand::Next,
        },
        &deck.remote,
    );

    assert_eq!(
        deck.remote.take_command(),
        Some(app_core::RemoteCommand::Next)
    );
    assert_eq!(
        deck.remote.take_command(),
        None,
        "取过一次就不该再有"
    );
}

// ── 遥控器侧:上报与撤权 ──

/// 被控端报来的状态落进镜像,进度与曲名从此读它。
#[test]
fn a_report_from_the_target_updates_the_mirror() {
    let (_ui, deck) = deck_window();
    deck.remote.assume_output("pc", "pc");

    crate::sync::remote::handle(
        &Event::RemoteState {
            from: "pc".to_owned(),
            state: Box::new(report(
                7_000,
                app_core::RemotePlayState::Playing,
            )),
        },
        &deck.remote,
    );

    let (known, position) =
        deck.remote.with_view(|view, now| {
            (view.is_known(), view.position_ms(now))
        });
    assert!(known, "该收下这条上报");
    assert!(
        position >= 7_000,
        "位置该从上报那个数起算,实得 {position}"
    );
}

/// 不是当前那台设备报来的,一概不收。
///
/// 换目标之后上一台的残余还会飘几条过来,收下它进度条就会跳到别人的歌上。
#[test]
fn a_report_from_another_device_is_ignored() {
    let (_ui, deck) = deck_window();
    deck.remote.assume_output("pc", "pc");

    crate::sync::remote::handle(
        &Event::RemoteState {
            from: "另一台".to_owned(),
            state: Box::new(report(
                7_000,
                app_core::RemotePlayState::Playing,
            )),
        },
        &deck.remote,
    );

    assert!(
        !deck.remote.with_view(|view, _| view.is_known()),
        "不该收下别人的上报"
    );
}

/// 失权就回到本机输出 —— 用户得知道手上这台不再管用了。
#[test]
fn revoking_control_returns_the_output_to_local() {
    let (_ui, deck) = deck_window();
    deck.remote.assume_output("pc", "pc");
    assert!(deck.remote.is_remote(), "先得真的切过去");

    crate::sync::remote::handle(
        &Event::ControlRevoked {
            by: "spare".to_owned(),
        },
        &deck.remote,
    );

    assert!(
        !deck.remote.is_remote(),
        "失权之后输出该回到本机"
    );
}

/// 遥控时封面跟着被控端的曲目走 —— 换歌那一拍,上一首那张立刻清掉。
///
/// 不清的话遥控器上停着进入遥控前那首的封面,歌名歌手却已经换了
/// (#102 之一)。第二次推同一首不再清:上报每秒一条,跟着它取图
/// 就是每秒一次下载,而界面上会看到封面每秒闪一下。
#[test]
fn the_cover_follows_the_remote_track_but_only_on_a_change()
{
    let (ui, deck) = deck_window();
    ui.global::<crate::Viz>().set_cover_art(image());
    deck.remote.assume_output("pc", "pc");
    crate::sync::remote::handle(
        &Event::RemoteState {
            from: "pc".to_owned(),
            state: Box::new(report(
                0,
                app_core::RemotePlayState::Playing,
            )),
        },
        &deck.remote,
    );

    crate::sync::remote::push_playback(&ui, &deck.remote);

    assert_eq!(
        ui.global::<crate::Viz>()
            .get_cover_art()
            .size()
            .width,
        0,
        "换歌那一拍该把上一首的封面清掉"
    );

    ui.global::<crate::Viz>().set_cover_art(image());
    crate::sync::remote::push_playback(&ui, &deck.remote);

    assert_eq!(
        ui.global::<crate::Viz>()
            .get_cover_art()
            .size()
            .width,
        1,
        "同一首歌再推一拍不该再清一次 —— 那是每秒一次的重取"
    );
}

/// 一张 1×1 的图。有没有图才是被测的东西,画的什么无关紧要。
fn image() -> slint::Image {
    slint::Image::from_rgba8(slint::SharedPixelBuffer::<
        slint::Rgba8Pixel,
    >::new(1, 1))
}

// ── 遥控器侧:控制条那一下改发命令,不碰本机播放器 ──

/// 输出设备不是本机时,播放键这一下**不落到本机播放器上**。
///
/// 落下去的话两台会同时出声 —— 而用户按的时候看的是手机,听的是 pc1,
/// 多出来的那一路声音他得回头找半天才知道是哪来的。
#[test]
fn a_remote_toggle_does_not_touch_the_local_transport() {
    let (ui, deck) = deck_window();
    deck.remote.assume_output("pc", "pc");
    crate::sync::remote::handle(
        &Event::RemoteState {
            from: "pc".to_owned(),
            state: Box::new(report(
                0,
                app_core::RemotePlayState::Playing,
            )),
        },
        &deck.remote,
    );
    ui.global::<Player>().set_is_playing(true);

    dispatch(&ui, &deck, Intent::TogglePlay);

    assert!(
        deck.remote.is_remote(),
        "输出该还在那台设备上"
    );
    assert!(
        deck.queue.borrow().current().is_none(),
        "遥控那一下不该在本机起播"
    );
}

/// 被控端退出之后,本机回到停止态 —— 不自动接着放。
///
/// 遥控期间本机播放器是空的,而本机那台状态机还停在进遥控之前的 `Playing`:
/// 回到本机那一拍不按停它,自动续播就当成「这一首放完了」接上下一首,
/// 于是平板上从 0:00 响起一首谁也没点过的歌(#102 之四)。
#[test]
fn coming_back_from_a_remote_device_leaves_the_local_transport_at_rest()
 {
    let (ui, deck) = deck_window();
    deck.remote.assume_output("pc", "pc");
    assert!(
        !deck.remote.took_local_edge(),
        "声音还在那台设备上,这不是回本机"
    );

    crate::sync::remote::handle(
        &Event::ControlRevoked {
            by: "pc".to_owned(),
        },
        &deck.remote,
    );

    assert!(
        deck.remote.took_local_edge(),
        "退回本机那一拍该认得出来"
    );
    assert!(
        !deck.remote.took_local_edge(),
        "只认一次 —— 每拍都认就是每秒把本机按停一次"
    );

    ui.global::<Player>().set_is_playing(true);
    rest_local(&ui, &deck);

    assert!(
        !ui.global::<Player>().get_is_playing(),
        "回到本机该停着"
    );
    assert!(
        matches!(
            deck.playback.borrow().state(),
            PlaybackState::Idle
        ),
        "状态机也得停 —— 不停的话自动续播每秒都会再撞进来"
    );
}

// ── 信令事件的落地 ──

/// 名册推到界面上,自己那一台被滤掉。
///
/// 留着自己的话,输出设备里会多一颗「输出到 我自己」—— 与常驻的「本机」是
/// 同一台,点下去是遥控自己。
#[test]
fn a_roster_event_reaches_the_device_list() {
    let (ui, deck) = deck_window();
    let roster =
        std::sync::Arc::new(std::sync::Mutex::new(
            syncplay::Roster::new("me".to_owned()),
        ));

    crate::sync::link::handle(
        Event::Roster(vec![device("me"), device("pc")]),
        &ui.as_weak(),
        &roster,
        &deck.remote,
    );

    assert_eq!(
        roster.lock().expect("名册锁").others().len(),
        1,
        "自己该被滤掉,只剩另一台"
    );
}

/// **无曲目、从个人页选远端**,被控端退出时遥控器要回本机(#102 F-003)。
///
/// 与上一条的差别全在入口:那条直接调 `Remote::select`,这条走用户真正走的路
/// —— 个人页那张卡上的 `OutputStrip` 芯片,经 `Shell.set-output` 回调进来,
/// 而且本机一首歌都没放过(`Player.has-track` 为假,控制条与抽屉整个不存在)。
/// 现场报的正是这条路径上收不到撤权,所以入口不能省成直调。
#[test]
fn a_revoke_comes_home_even_when_nothing_ever_played() {
    use slint::{ModelRc, VecModel};

    let (ui, deck) = deck_window();
    // 回调只有 `bind` 接得上 —— fixture 里那副 Deck 是 `detached` 的。
    // 输出芯片接在音乐页(选设备 = 迁移,要从 `Deck` 里凑迁过去的那一份)。
    crate::sync::remote::bind(&ui, &deck.remote);
    bind_session(&ui, &deck);
    ui.global::<crate::Shell>().set_devices(ModelRc::new(
        VecModel::from(vec![crate::DeviceRow {
            id: "pc".into(),
            name: "pc1".into(),
        }]),
    ));
    ui.global::<crate::Shell>().set_current_tab(2);

    assert!(
        !ui.global::<Player>().get_has_track(),
        "这一条测的就是没放过歌的冷启动"
    );

    // 无头下条件元素惰性实例化,查一次把个人页逼出来。
    let chip = i_slint_backend_testing::ElementHandle::find_by_accessible_label(
        &ui, "输出到 pc1",
    )
    .next()
    .expect("个人页上该有 pc1 那颗输出芯片");
    chip.invoke_accessible_default_action();
    // 选设备是一次迁移(#137 ③):本机什么都没在放,没有东西可迁,只剩「停源、
    // 确认」两步。停源是本机那一步,交给 UI 线程执行 —— 无头测试里事件循环
    // 不转,这里替它把收件箱倒一遍。
    while let Some(effect) = deck.remote.take_local_effect()
    {
        run_local(&ui, &deck, effect);
    }

    assert!(
        deck.remote.is_remote(),
        "点了芯片、本机停源确认之后,输出该交给那台设备"
    );

    crate::sync::remote::handle(
        &Event::ControlRevoked {
            by: "pc".to_owned(),
        },
        &deck.remote,
    );

    assert!(
        !deck.remote.is_remote(),
        "被控端退出之后输出该回本机,而不是停在「状态已过期」"
    );
    assert_eq!(
        ui.global::<crate::Shell>().get_output_id(),
        "",
        "芯片那一行也要跟着回本机 —— 现场看到的正是它还亮在 pc1 上"
    );
}

/// 被控端失联十五秒,遥控器自己回本机(#102 F-003 的出口)。
///
/// 现场那次撤权丢在路上:服务端删了槽位、`try_send` 一发没送到,而遥控器这条
/// socket 好好的、不会重连,于是重连那条自愈也走不到。它停在「遥控: 状态已过期」
/// 一分钟,芯片还亮在 pc1 上,本机什么也放不了。这一条是那种情况下唯一的出口。
///
/// 时钟由 `give_up_if_lost_at` 收着 —— 十五秒的判断不能靠真的睡十五秒。
#[test]
fn a_silent_target_hands_the_output_back_after_fifteen_seconds()
 {
    let (ui, deck) = deck_window();
    crate::sync::remote::bind(&ui, &deck.remote);
    deck.remote.assume_output("pc", "pc1");

    let now = crate::sync::remote::now_ms();
    crate::sync::remote::handle(
        &Event::RemoteState {
            from: "pc".to_owned(),
            state: Box::new(report(
                1_000,
                app_core::RemotePlayState::Playing,
            )),
        },
        &deck.remote,
    );

    // 轮询每一拍都问一次边沿,这里照它的顺序走一拍 —— 不问的话
    // `was_remote` 一直是假,下面那条边沿断言测的就不是同一件事了。
    assert!(
        !deck.remote.took_local_edge(),
        "声音还在那台设备上,这不是回本机"
    );

    assert!(
        !deck.remote.give_up_if_lost_at(now + 14_000),
        "才十四秒,抖一下不该把声音抢回本机"
    );
    assert!(
        deck.remote.is_remote(),
        "没失联就该还在那台设备上"
    );

    assert!(
        deck.remote.give_up_if_lost_at(now + 16_000),
        "十五秒过了就该收回来"
    );
    assert!(
        !deck.remote.is_remote(),
        "输出该回本机 —— 这正是现场卡住的那一步"
    );
    assert_eq!(
        ui.global::<crate::Shell>().get_output_id(),
        "",
        "芯片也要跟着灭 —— 现场看到的是它还亮着"
    );
    assert!(
        deck.remote.took_local_edge(),
        "回本机那一拍要认得出来,自动续播才不会顺手起播"
    );
    assert!(
        !deck.remote.give_up_if_lost_at(now + 99_000),
        "已经回本机了就不该再收一次,否则每秒弹一条提示"
    );
}

/// 失联回本机时要把持权记录一起交出去,不然重连时它会把被控端重新锁上(#118)。
///
/// 回本机此前只改了界面这一侧:客户端手上那份带代次的持权记录原样留着,
/// 遥控器自己的信令哪天重连一次,就拿着它去续权 —— 服务端槽位没换人就续上了,
/// 被控端又挂起「正被遥控」,而遥控器这头早就是本机输出、谁也不在遥控它。
/// 客户端那一半(交出记录之后重连不再续权)见 `syncplay/tests/remote.rs`。
#[test]
fn giving_up_on_a_lost_target_releases_the_claim() {
    let (_ui, deck) = deck_window();
    deck.remote.assume_output("pc", "pc1");
    let now = crate::sync::remote::now_ms();
    crate::sync::remote::handle(
        &Event::RemoteState {
            from: "pc".to_owned(),
            state: Box::new(report(
                1_000,
                app_core::RemotePlayState::Playing,
            )),
        },
        &deck.remote,
    );
    let before = deck.remote.releases();

    assert!(deck.remote.give_up_if_lost_at(now + 16_000));

    assert_eq!(
        deck.remote.releases(),
        before + 1,
        "回本机那一下要把持权交给客户端去忘掉"
    );
}

/// 服务端说没人在遥控本机,横幅与锁定态就该立刻撤掉(#102 F-004)。
///
/// 锁定态此前只有用户按「退出被遥控」才清,于是槽位一旦在本机不知情时没了,
/// 这台就挂着假横幅、锁着本地播放,而横幅上那台设备早就不管它了。
#[test]
fn being_told_nobody_is_in_control_unlocks_the_local_transport()
 {
    let (ui, deck) = deck_window();
    crate::sync::remote::handle(
        &Event::ControlledBy {
            device: device("phone"),
        },
        &deck.remote,
    );
    assert!(
        deck.remote.is_controlled(),
        "接管之后本机该是锁定的"
    );

    crate::sync::remote::handle(
        &Event::NotControlled,
        &deck.remote,
    );

    assert!(
        !deck.remote.is_controlled(),
        "服务端都说没人遥控了,还锁着就是把本机白白废掉"
    );
    let _ = &ui;
}

// ── 选设备 = 迁移当前播放(#137 ③)──

/// 把一个 future 跑到底。这里的 future 不会真的挂起(准备闭包是现成的值)。
fn run<F: core::future::Future>(future: F) -> F::Output {
    use core::task::{Context, Poll};

    let mut cx =
        Context::from_waker(core::task::Waker::noop());
    let mut future = Box::pin(future);
    loop {
        if let Poll::Ready(value) =
            future.as_mut().poll(&mut cx)
        {
            return value;
        }
    }
}

/// 本机正在放 `track()`:队列、执行副本(服务端队列 7 的第 3 版、条目 12)、
/// 播放状态机都在 Playing 上。
fn playing_locally(deck: &Deck) {
    deck.queue.borrow_mut().replace(vec![track()], 0);
    deck.execution.adopt(7, 3, vec![12]);
    run(app_core::play(
        &deck.playback,
        track(),
        |_| async { Ok::<(), String>(()) },
        |()| {},
    ));
}

/// 这一次迁移的操作号:从发给目标的那条准备命令里读。
fn operation_of(deck: &Deck) -> String {
    deck.remote
        .routed()
        .into_iter()
        .find_map(|(_, cmd)| match cmd {
            app_core::RemoteCommand::Prepare {
                operation_id,
                ..
            } => Some(operation_id),
            _ => None,
        })
        .expect("该有一条准备命令")
}

/// 一条带迁移回话的上报。
fn acking(
    operation_id: &str,
    phase: app_core::OperationPhase,
) -> app_core::RemoteStateDto {
    app_core::RemoteStateDto {
        operation: Some(app_core::OperationAckDto {
            operation_id: operation_id.to_owned(),
            phase,
            position_ms: None,
            reason: None,
        }),
        ..report(0, app_core::RemotePlayState::Idle)
    }
}

/// 本机在放时选 pc:先向服务端登记、叫 pc 准备当前这一条 —— 本机一个字节都不动,
/// 输出也还没换(目标没确认之前不换)。
#[test]
fn selecting_a_device_prepares_it_and_leaves_local_playback_alone()
 {
    let (ui, deck) = deck_window();
    playing_locally(&deck);

    select_output(&ui, &deck, "pc");

    assert_eq!(
        deck.remote.group_ops().len(),
        1,
        "该先登记一次: {:?}",
        deck.remote.group_ops()
    );
    let routed = deck.remote.routed();
    assert!(
        matches!(
            routed.as_slice(),
            [(to, app_core::RemoteCommand::Prepare {
                queue_id: 7,
                revision: 3,
                entry_id: 12,
                ..
            })] if to == "pc"
        ),
        "该叫 pc 准备队列 7@3 的条目 12: {routed:?}"
    );
    assert!(
        !deck.remote.is_remote(),
        "目标没确认之前输出不换"
    );
    assert!(deck.remote.is_moving());
    assert!(
        matches!(
            deck.playback.borrow().state(),
            PlaybackState::Playing(_)
        ),
        "准备阶段本机照旧在放"
    );
}

/// 迁移那几秒控制条**不消失**,一直是迁过去的那一首。
///
/// 从前选完设备镜像一清,下一拍 `has-track` 就被置假,控制条连同抽屉里刚点的
/// 芯片一起销毁(#137 已核实的结构问题 2)。
#[test]
fn the_control_bar_stays_on_the_moving_track() {
    let (ui, deck) = deck_window();
    playing_locally(&deck);
    select_output(&ui, &deck, "pc");
    ui.global::<Player>().set_has_track(false);

    crate::sync::remote::push_moving(&ui, &deck.remote);

    assert!(ui.global::<Player>().get_has_track());
    assert_eq!(
        ui.global::<Player>().get_now_id(),
        track().id
    );
    assert!(
        ui.global::<Player>()
            .get_playback_text()
            .contains("正在切到"),
        "{}",
        ui.global::<Player>().get_playback_text()
    );
}

/// 整条路走一遍:pc 准备好 → 本机停下 → 叫 pc 从本机停下的位置开始 → pc 确认
/// → 提交,输出这才换到 pc。本机停下之后不再出声,也不会自己接着放。
#[test]
fn a_ready_target_stops_local_playback_then_starts_from_there()
 {
    let (ui, deck) = deck_window();
    playing_locally(&deck);
    select_output(&ui, &deck, "pc");
    let op = operation_of(&deck);

    crate::sync::remote::handle(
        &Event::RemoteState {
            from: "pc".to_owned(),
            state: Box::new(acking(
                &op,
                app_core::OperationPhase::Prepared,
            )),
        },
        &deck.remote,
    );
    let stop = deck
        .remote
        .take_local_effect()
        .expect("准备好之后该轮到本机停");
    assert!(
        matches!(stop, app_core::Effect::Stop { .. }),
        "{stop:?}"
    );
    run_local(&ui, &deck, stop);

    assert!(
        matches!(
            deck.playback.borrow().state(),
            PlaybackState::Idle
        ),
        "本机该停下"
    );
    let start = deck.remote.routed().pop();
    assert!(
        matches!(
            start,
            Some((ref to, app_core::RemoteCommand::Start {
                position_ms: 0,
                ..
            })) if to == "pc"
        ),
        "该叫 pc 从本机停下的位置(测试里没有声卡,是 0)开始: {start:?}"
    );
    assert!(
        !deck.remote.is_remote(),
        "pc 确认开始之前输出不换"
    );

    crate::sync::remote::handle(
        &Event::RemoteState {
            from: "pc".to_owned(),
            state: Box::new(acking(
                &op,
                app_core::OperationPhase::Started,
            )),
        },
        &deck.remote,
    );

    assert!(
        deck.remote
            .group_ops()
            .iter()
            .any(|op| op.starts_with("commit")),
        "该提交: {:?}",
        deck.remote.group_ops()
    );
    assert_eq!(
        deck.remote.target_id().as_deref(),
        Some("pc")
    );
    assert!(!deck.remote.is_moving());
}

/// 迁移那几秒按下一首不落在任何一台上。
#[test]
fn transport_is_held_while_the_output_is_moving() {
    let (ui, deck) = deck_window();
    playing_locally(&deck);
    select_output(&ui, &deck, "pc");

    let outcome = dispatch(&ui, &deck, Intent::Next);

    assert_eq!(
        outcome,
        Dispatched::Blocked("正在切换输出")
    );
}

/// 本机这一批还没同步到服务端:不迁 —— 目标只能按服务端的标识取执行副本。
#[test]
fn an_unsynced_local_queue_is_not_moved() {
    let (ui, deck) = deck_window();
    playing_locally(&deck);
    deck.execution.detach();

    select_output(&ui, &deck, "pc");

    assert!(!deck.remote.is_moving());
    assert_eq!(deck.remote.routed(), Vec::new());
}

/// 遥控着 pc 时选回本机:本机准备,pc 先不停 —— 本机没备好之前停 pc 就是一段空白。
#[test]
fn moving_back_home_prepares_locally_before_stopping_the_remote()
 {
    let (ui, deck) = deck_window();
    deck.remote.assume_output("pc", "pc1");
    crate::sync::remote::handle(
        &Event::RemoteState {
            from: "pc".to_owned(),
            state: Box::new(report(
                30_000,
                app_core::RemotePlayState::Playing,
            )),
        },
        &deck.remote,
    );

    select_output(&ui, &deck, "");

    assert!(
        matches!(
            deck.remote.take_local_effect(),
            Some(app_core::Effect::Prepare { .. })
        ),
        "该先在本机准备"
    );
    assert!(
        !deck.remote.routed().iter().any(
            |(_, cmd)| matches!(
                cmd,
                app_core::RemoteCommand::Stop { .. }
            )
        ),
        "本机没备好之前不该叫 pc 停"
    );
}

// ── 被叫去迁移的那一端 ──

/// 没备过的那一次开始不了:报失败,不临时现取 —— 重启过的设备收到旧的开始就该这样。
#[test]
fn a_start_that_was_never_prepared_is_answered_with_a_failure()
 {
    let (ui, deck) = deck_window();

    execute(
        &ui,
        &deck,
        app_core::RemoteCommand::Start {
            operation_id: "op".to_owned(),
            position_ms: 1_000,
            playing: true,
        },
    );

    let answer = deck.member.ack().expect("该回一句");
    assert_eq!(
        answer.phase,
        app_core::OperationPhase::Failed
    );
    assert!(
        deck.queue.borrow().current().is_none(),
        "没备过就不该起播"
    );
}

/// 叫停:本机停下,回话里带停下的位置;重发的停止照报同一个位置。
#[test]
fn a_stop_command_stops_the_local_output_and_says_where() {
    let (ui, deck) = deck_window();
    playing_locally(&deck);

    execute(
        &ui,
        &deck,
        app_core::RemoteCommand::Stop {
            operation_id: "op".to_owned(),
        },
    );
    let first = deck.member.ack().expect("该回一句");
    execute(
        &ui,
        &deck,
        app_core::RemoteCommand::Stop {
            operation_id: "op".to_owned(),
        },
    );

    assert_eq!(
        first.phase,
        app_core::OperationPhase::Stopped
    );
    assert!(first.position_ms.is_some());
    assert_eq!(deck.member.ack(), Some(first));
    assert!(matches!(
        deck.playback.borrow().state(),
        PlaybackState::Idle
    ));
}

/// 上报里带着最近一次迁移步骤的回话 —— 遥控器靠它往下走。
#[test]
fn the_report_carries_the_last_migration_answer() {
    let (ui, deck) = deck_window();
    execute(
        &ui,
        &deck,
        app_core::RemoteCommand::Stop {
            operation_id: "op".to_owned(),
        },
    );

    let snap = snapshot(&deck);

    assert_eq!(
        snap.operation.map(|answer| answer.operation_id),
        Some("op".to_owned())
    );
}

/// 同一台同一批再点一首:原样用上一次发布的那一版,不再发一个新版本。
#[test]
fn the_same_batch_for_the_same_device_reuses_its_revision()
{
    let (_ui, deck) = deck_window();
    let batch =
        vec![track_with_id("a"), track_with_id("b")];
    let published = api::QueueRefDto {
        queue_id: 7,
        revision: 3,
        entry_ids: vec![1, 2],
    };
    deck.remote.note_published(
        "pc",
        &batch,
        published.clone(),
    );

    assert_eq!(
        deck.remote.published_for("pc", &batch),
        Some(published)
    );
    assert_eq!(
        deck.remote.published_for("tablet", &batch),
        None,
        "换一台就不是同一个队列"
    );
    assert_eq!(
        deck.remote.published_for("pc", &batch[..1]),
        None,
        "换一批就得重新发布"
    );
}
