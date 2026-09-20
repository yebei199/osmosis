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
        queue: vec![track()],
        queue_index: 0,
        volume: 0.5,
        sent_at: position_ms,
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

    execute(
        &ui,
        &deck,
        app_core::RemoteCommand::Play {
            tracks: batch.clone(),
            index: 1,
        },
    );

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
    execute(
        &ui,
        &deck,
        app_core::RemoteCommand::Play {
            tracks: vec![
                track_with_id("a"),
                track_with_id("b"),
            ],
            index: 0,
        },
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
    execute(
        &ui,
        &deck,
        app_core::RemoteCommand::Play {
            tracks: vec![
                track_with_id("a"),
                track_with_id("b"),
            ],
            index: 1,
        },
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
    deck.remote.select("pc", "pc");

    crate::sync::remote::handle(
        &Event::RemoteState {
            from: "pc".to_owned(),
            state: report(
                7_000,
                app_core::RemotePlayState::Playing,
            ),
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
    deck.remote.select("pc", "pc");

    crate::sync::remote::handle(
        &Event::RemoteState {
            from: "另一台".to_owned(),
            state: report(
                7_000,
                app_core::RemotePlayState::Playing,
            ),
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
    deck.remote.select("pc", "pc");
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
    deck.remote.select("pc", "pc");
    crate::sync::remote::handle(
        &Event::RemoteState {
            from: "pc".to_owned(),
            state: report(
                0,
                app_core::RemotePlayState::Playing,
            ),
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
    deck.remote.select("pc", "pc");
    crate::sync::remote::handle(
        &Event::RemoteState {
            from: "pc".to_owned(),
            state: report(
                0,
                app_core::RemotePlayState::Playing,
            ),
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
    deck.remote.select("pc", "pc");
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

// ── 同播事件的落地 ──

/// 名册推到界面上,自己那一台被滤掉。
///
/// 留着自己的话,设备列表里会出现一行「推给我自己」——它甚至连得成功,
/// 只是声音绕一圈回到同一个扬声器。
#[test]
fn a_roster_event_reaches_the_device_list() {
    let (ui, deck) = deck_window();
    let roster =
        std::sync::Arc::new(std::sync::Mutex::new(
            syncplay::Roster::new("me".to_owned()),
        ));
    let role = std::sync::Arc::new(std::sync::Mutex::new(
        syncplay::Role::Alone,
    ));

    crate::sync::syncplay::handle(
        Event::Roster(vec![device("me"), device("pc")]),
        &ui.as_weak(),
        &roster,
        &role,
        &deck.player,
        &deck.remote,
    );

    assert_eq!(
        roster.lock().expect("名册锁").others().len(),
        1,
        "自己该被滤掉,只剩另一台"
    );
}

/// 失败走提示,**不写同播状态行**。
///
/// 写进状态行就没人会重算它,那句话会一直挂到角色碰巧变一次为止 ——
/// 而角色此刻一动没动,那一行依然为真。
#[test]
fn a_failure_event_does_not_rewrite_the_role_line() {
    let (ui, deck) = deck_window();
    let roster =
        std::sync::Arc::new(std::sync::Mutex::new(
            syncplay::Roster::new("me".to_owned()),
        ));
    let role = std::sync::Arc::new(std::sync::Mutex::new(
        syncplay::Role::Alone,
    ));
    let before =
        ui.global::<crate::Shell>().get_sync_text();

    crate::sync::syncplay::handle(
        Event::Failed("连不上".to_owned()),
        &ui.as_weak(),
        &roster,
        &role,
        &deck.player,
        &deck.remote,
    );

    assert_eq!(
        ui.global::<crate::Shell>().get_sync_text(),
        before,
        "失败是这一刻的事,不该写进角色那一行"
    );
    assert_eq!(
        *role.lock().expect("角色锁"),
        syncplay::Role::Alone,
        "角色一动没动"
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
    crate::sync::remote::bind(&ui, &deck.remote);
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

    assert!(
        deck.remote.is_remote(),
        "点了芯片就该把输出交给那台设备"
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
    deck.remote.select("pc", "pc1");

    let now = crate::sync::remote::now_ms();
    crate::sync::remote::handle(
        &Event::RemoteState {
            from: "pc".to_owned(),
            state: report(
                1_000,
                app_core::RemotePlayState::Playing,
            ),
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
