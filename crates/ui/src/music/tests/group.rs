//! 组里的点歌与控制(#142):本机在组里时,每个入口都只改服务端的全局状态。
//!
//! 入口一律用 Slint 的回调(`invoke_play`、`invoke_toggle_play`、…),与 `dispatch.rs`
//! 那一组同一个理由:测的是用户真正走的那条路。测试里没有服务端,发出去的意图记在
//! `Group::intents` 上。

use app_core::{GroupNowDto, GroupStateDto, LoopModeDto};
use similar_asserts::assert_eq;

use super::super::fixtures::*;
use super::super::*;
use crate::{Player, Shell, Viz};

/// 把控制条、输出设备那一排、队列页接到窗口上。
fn wire(ui: &MainWindow, deck: &Deck) {
    bind_play(ui, deck);
    bind_controls(ui, deck);
    bind_volume(ui, deck);
    bind_seek(ui, deck);
    bind_outputs(ui, deck);
    queuepage::bind(ui, deck);
}

fn batch_of(deck: &Deck, ids: &[&str]) -> Vec<TrackDto> {
    let batch: Vec<TrackDto> =
        ids.iter().map(|id| track_with_id(id)).collect();
    *deck.tracks.borrow_mut() = batch.clone();
    batch
}

/// 组的一版状态:成员是本机(`me`)与 pc,出声的是 `outputs`,在放第 12 条、200 秒长。
fn state(outputs: &[&str], playing: bool) -> GroupStateDto {
    let mut track = track_with_id("x");
    track.duration_ms = 200_000;
    GroupStateDto {
        version: 3,
        members: vec!["me".to_owned(), "pc".to_owned()],
        outputs: outputs
            .iter()
            .map(|id| (*id).to_owned())
            .collect(),
        clock_epoch: 1,
        now: Some(GroupNowDto {
            queue_id: 7,
            revision: 2,
            entry_id: 12,
            track,
            anchor_us: 0,
            position_us: 0,
            playing,
            next: None,
            shuffled: false,
            loop_mode: LoopModeDto::Off,
        }),
    }
}

/// 只当遥控器的手机在列表里点歌:发一条组意图,本机一首都不放(AC-1)。
#[test]
fn a_tap_in_the_group_goes_to_the_server_not_the_local_player()
 {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    batch_of(&deck, &["a", "b", "c"]);
    deck.group.assume(Some(state(&["pc"], true)));

    ui.global::<Player>().invoke_play("b".into());

    assert_eq!(deck.group.intents(), vec!["play 1"]);
    assert!(
        deck.queue.borrow().current().is_none(),
        "在组里点歌不在本机放"
    );
}

/// 出声的那一台自己点歌也一样只发意图:谁点都改同一份全局状态。
#[test]
fn a_sounding_member_also_goes_through_the_server() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    batch_of(&deck, &["a", "b"]);
    deck.group.assume(Some(state(&["me"], true)));

    ui.global::<Player>().invoke_play("a".into());

    assert_eq!(deck.group.intents(), vec!["play 0"]);
}

/// 同一首的意图还在路上:不再发第二次。
#[test]
fn a_second_tap_on_the_same_track_is_not_sent_again() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    batch_of(&deck, &["a", "b"]);
    deck.group.assume(Some(state(&["pc"], true)));

    ui.global::<Player>().invoke_play("b".into());
    ui.global::<Player>().invoke_play("b".into());

    assert_eq!(deck.group.intents(), vec!["play 1"]);
}

/// ⏯ 按全局状态翻:在放就暂停,停着就继续;下一首、上一首照发(AC-2)。
#[test]
fn transport_keys_follow_the_global_state() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(state(&["pc"], true)));

    ui.global::<Player>().invoke_toggle_play();
    ui.global::<Player>().invoke_next_track();
    ui.global::<Player>().invoke_prev_track();

    assert_eq!(
        deck.group.intents(),
        vec!["Pause", "Next", "Prev"]
    );
}

/// 组里停着时 ⏯ 是继续。
#[test]
fn toggling_a_paused_group_resumes_it() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(state(&["pc"], false)));

    ui.global::<Player>().invoke_toggle_play();

    assert_eq!(deck.group.intents(), vec!["Resume"]);
}

/// 拖进度按组里那一首的曲长换算:本机这时手上没歌也拖得动。
#[test]
fn seeking_uses_the_group_track_length() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(state(&["pc"], true)));

    ui.global::<Player>().invoke_seek(0.5);

    assert_eq!(
        deck.group.intents(),
        vec!["Seek { position_ms: 100000 }"]
    );
}

/// 音量每台各自调:在组里也落在本机,不发意图。
#[test]
fn volume_stays_on_this_device() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(state(&["pc"], true)));

    ui.global::<Player>().invoke_volume_changed(0.3);

    assert!(deck.group.intents().is_empty());
    assert!(
        (ui.global::<Player>().get_volume() - 0.3).abs()
            < 1e-6
    );
}

/// 记下推给系统媒体控件的每一份。
struct Recorded(
    std::rc::Rc<core::cell::RefCell<Vec<bool>>>,
);

impl crate::media::MediaControls for Recorded {
    fn publish(&self, now: &crate::media::NowPlaying) {
        self.0.borrow_mut().push(now.remote);
    }
}

/// 只当遥控器时拖音量,报出去的仍是遥控那一份(#150)。
///
/// 从前这一下推的是本机那份(`remote: false`,状态「停」),与每秒轮询推的遥控那份来回
/// 翻:安卓上就是 stopService 与 startForegroundService 交替,stop 一旦落在服务调
/// startForeground 之前,系统当场杀进程。
#[test]
fn a_silent_member_dragging_the_volume_keeps_reporting_the_remote()
 {
    let (ui, deck) = deck_window();
    let published = std::rc::Rc::default();
    let deck = Deck {
        media: std::rc::Rc::new(crate::media::Bridge::new(
            Box::new(Recorded(std::rc::Rc::clone(
                &published,
            ))),
            std::sync::Arc::default(),
        )),
        ..deck
    };
    wire(&ui, &deck);
    deck.group.assume(Some(state(&["pc"], false)));
    crate::media::push_remote(
        &ui,
        &deck.group,
        &deck.media,
    );

    for level in [0.2, 0.5, 0.0] {
        ui.global::<Player>().invoke_volume_changed(level);
    }
    ui.global::<Player>().invoke_toggle_play();

    assert_eq!(*published.borrow(), vec![true]);
}

/// 随机与循环是全局状态的一部分:在组里拨它们发意图。
#[test]
fn shuffle_and_loop_go_to_the_group() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(state(&["pc"], true)));

    ui.global::<Player>().invoke_shuffle_toggled();
    ui.global::<Player>().invoke_loop_cycled();

    assert_eq!(
        deck.group.intents(),
        vec!["Shuffle { on: true }", "Loop { mode: All }"]
    );
}

/// 在组里点一台设备:改成只在它上面出声;点本机就是本机出声。
#[test]
fn picking_a_device_in_the_group_sets_the_outputs() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(state(&["pc"], true)));

    ui.global::<Shell>().invoke_set_output("pc".into());
    ui.global::<Shell>().invoke_set_output("".into());

    assert_eq!(
        deck.group.intents(),
        vec![r#"outputs ["pc"]"#, r#"outputs ["me"]"#]
    );
}

/// 「+」在正在出声的那几台上加上一台:本机与 pc 一起出声(AC-5)。
#[test]
fn the_plus_key_adds_an_output() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(state(&["pc"], true)));

    ui.global::<Shell>().invoke_toggle_member("".into());

    assert_eq!(
        deck.group.intents(),
        vec![r#"outputs ["pc", "me"]"#]
    );
}

/// 「+」再按一下就移出:pc 不再出声,本机接着放。
#[test]
fn the_plus_key_removes_an_output_again() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(state(&["pc", "me"], true)));

    ui.global::<Shell>().invoke_toggle_member("pc".into());

    assert_eq!(
        deck.group.intents(),
        vec![r#"outputs ["me"]"#]
    );
}

/// 独奏时点本机:本来就在本机,什么都不发。
#[test]
fn picking_this_device_while_solo_does_nothing() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);

    ui.global::<Shell>().invoke_set_output("".into());

    assert!(deck.group.intents().is_empty());
}

/// 独奏、本机什么都没在放时点 pc:建组,没有种子(组从空的开始)。
#[test]
fn picking_a_device_while_solo_starts_a_group() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);

    ui.global::<Shell>().invoke_set_output("pc".into());

    assert_eq!(
        deck.group.intents(),
        vec![r#"outputs ["pc"]"#]
    );
}

/// 队列页在组里点一条:切到组队列的那一条(AC-1 的队列页入口)。
#[test]
fn the_queue_page_in_the_group_picks_an_entry() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(state(&["pc"], true)));

    ui.global::<Viz>().invoke_queue_pick("15".into());

    assert_eq!(deck.group.intents(), vec!["pick 15"]);
}

/// 在组里就不自己续播、不自己上报起播:放哪一首只听全局状态,起播由服务端记。
#[test]
fn a_member_leaves_advancing_to_the_group() {
    let (_ui, deck) = deck_window();
    assert!(!follows_the_group(&deck));

    deck.group.assume(Some(state(&["me"], true)));

    assert!(follows_the_group(&deck));
    assert!(!is_silent_member(&deck), "本机在出声");

    deck.group.assume(Some(GroupStateDto {
        version: 4,
        ..state(&["pc"], true)
    }));
    assert!(is_silent_member(&deck), "本机只当遥控器");
}

/// 组里正在放的那一首再点:与本机同一个判据,是多余的一下,不再发(#142 F-1)。服务端几毫秒
/// 就回了应答,只看「意图在路上」挡不住连点的第二下。
#[test]
fn tapping_the_track_the_group_is_playing_is_redundant() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    batch_of(&deck, &["a", "x"]);
    deck.group.assume(Some(state(&["pc"], true)));

    ui.global::<Player>().invoke_play("x".into());

    assert!(deck.group.intents().is_empty());
}

/// 组暂停着:出声设备的 ⏯ 画成「播放」,不看本机播放器(它在组里一直没按暂停)(#142 F-4)。
#[test]
fn the_play_key_follows_the_group_state() {
    let (ui, deck) = deck_window();
    ui.global::<Player>().set_is_playing(true);
    deck.group.assume(Some(state(&["me"], false)));

    align(&ui, &deck);

    assert!(!ui.global::<Player>().get_is_playing());
}

/// 出声设备放完了此刻那一首:报一次 advance,同一份不报第二次(AC-9)。
#[test]
fn a_finished_output_reports_once() {
    let (ui, deck) = deck_window();
    deck.group.assume(Some(state(&["me"], true)));

    deck.group.finished(&ui);
    deck.group.finished(&ui);

    assert_eq!(deck.group.intents(), vec!["advance 12"]);
}

/// 只当遥控器的不出声,没有「放完」可报。
#[test]
fn a_silent_member_never_reports_finishing() {
    let (ui, deck) = deck_window();
    deck.group.assume(Some(state(&["pc"], true)));

    deck.group.finished(&ui);

    assert!(deck.group.intents().is_empty());
}

/// 队列页点的正是组此刻在放的那一条:与列表同一个判据,是多余的一下,不再发(#142 N-1)。
#[test]
fn picking_the_entry_the_group_is_playing_is_redundant() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(state(&["pc"], true)));

    ui.global::<Viz>().invoke_queue_pick("12".into());
    ui.global::<Viz>().invoke_queue_pick("12".into());

    assert!(deck.group.intents().is_empty());
}

/// 出声设备丢了音频焦点(比如切后台时桌面抢了焦点)、再拿回来:都不向组发意图,组照放
/// (#142 AC-10)。
#[test]
fn losing_audio_focus_in_the_group_sends_nothing() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(state(&["me"], true)));

    ui.global::<Player>().invoke_focus_changed(false);
    ui.global::<Player>().invoke_focus_changed(true);

    assert!(deck.group.intents().is_empty());
}

/// 焦点回来(系统的 GAIN 回调,或者永久丢失之后重新申请当场批准)在组里清掉「丢了焦点」,
/// 并记下这一首要按状态此刻的位置重新起,不发意图(#142 N-4)。
#[test]
fn regaining_focus_in_the_group_restarts_at_the_group_position()
 {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(state(&["me"], true)));

    ui.global::<Player>().invoke_focus_changed(false);
    assert!(deck.alignment.focus_lost());
    assert!(deck.alignment.restarts_next());

    ui.global::<Player>().invoke_focus_changed(true);
    assert!(
        !deck.alignment.focus_lost(),
        "焦点回来了就不再停着"
    );
    assert!(
        deck.alignment.restarts_next(),
        "照状态此刻的位置重新起,不在停下的流上接着追"
    );
    assert!(deck.group.intents().is_empty());
}

/// 组在别的设备上放、本机不在组里:成员与出声都是 `outputs`。
fn elsewhere(outputs: &[&str]) -> GroupStateDto {
    let mut state = state(outputs, true);
    state.members.clone_from(&state.outputs);
    state
}

/// 后来者加入(#149):组在 a、b 上放,独奏的本机点横幅上的「加入」,发出去的是
/// a、b 再加上本机 —— 原来出声的一台都不少,也不带种子(组接着放它自己的那一首)。
#[test]
fn a_late_device_joins_without_kicking_anyone() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    crate::sync::group::bind(&ui, &deck.group);
    deck.group.assume(Some(elsewhere(&["a", "b"])));

    ui.global::<Shell>().invoke_join_group();

    assert_eq!(
        deck.group.intents(),
        vec![r#"outputs ["a", "b", "me"]"#]
    );
}

/// 没有组在放就没有什么可加入的:什么都不发。
#[test]
fn there_is_nothing_to_join_without_a_group() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    crate::sync::group::bind(&ui, &deck.group);

    ui.global::<Shell>().invoke_join_group();

    assert!(deck.group.intents().is_empty());
}

/// 组已存在时独奏的本机按「+」:在组的出声设备上追加,不拿本机播放拼一份新的去覆盖
/// (#149)。已经在出声的那台再按也不会被移出 —— 独奏时芯片上它不亮,那颗键写的是「加入」。
#[test]
fn the_plus_key_while_solo_adds_to_the_existing_group() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(elsewhere(&["a", "b"])));

    ui.global::<Shell>().invoke_toggle_member("c".into());
    ui.global::<Shell>().invoke_toggle_member("a".into());

    assert_eq!(
        deck.group.intents(),
        vec![
            r#"outputs ["a", "b", "c"]"#,
            r#"outputs ["a", "b"]"#
        ]
    );
}

/// 组已存在时独奏的本机选一台设备:同样是追加,原有的出声设备不被挤掉(#149)。
#[test]
fn picking_a_device_while_solo_keeps_the_existing_outputs()
{
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(elsewhere(&["a", "b"])));

    ui.global::<Shell>().invoke_set_output("c".into());

    assert_eq!(
        deck.group.intents(),
        vec![r#"outputs ["a", "b", "c"]"#]
    );
}

/// 加入入口只在组**在播**时出现(#149 AC-1):组从在播转为暂停,独奏设备上的入口随下一版
/// 状态收掉。出声设备都关了、组只是停着时,不该在每台独奏设备上一直挂一条横幅。
#[test]
fn the_join_entry_goes_away_when_the_group_pauses() {
    let (_ui, deck) = deck_window();

    deck.group.assume(Some(elsewhere(&["a"])));
    let (banner, joinable) = deck.group.banner();
    assert!(joinable, "组在播,该有入口");
    assert_eq!(banner, "组里正在播放: 歌 x");

    let mut paused = elsewhere(&["a"]);
    paused.version = 4;
    paused.now.as_mut().expect("在放").playing = false;
    deck.group.assume(Some(paused));

    assert_eq!(
        deck.group.banner(),
        (String::new(), false),
        "组暂停:不挂入口,独奏时横幅照旧不出现"
    );
}

/// 别的成员远程调本机的音量(#151):本机照自己拖滑块那样应用、存盘,不发任何组意图。
#[test]
fn a_remote_volume_change_is_applied_and_saved() {
    let _file = super::dispatch::SETTINGS_FILE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (ui, deck) = deck_window_pumped();
    wire(&ui, &deck);
    deck.group.assume(Some(state(&["me", "pc"], true)));

    // 收到信令那一步(`Group::handle`)只是把这一下送上 UI 线程;测试后端没有进程级的
    // 事件循环代理,送不过去,所以直接走 UI 线程上那一半。
    crate::sync::group::apply_remote_volume(&ui, 0.2);
    super::dispatch::settle_volume_save();

    assert!(
        (ui.global::<Player>().get_volume() - 0.2).abs()
            < 1e-6
    );
    assert!(
        (api::settings::load().volume - 0.2).abs() < 1e-6,
        "远程调的音量该存进本机设置"
    );
    assert!(deck.group.intents().is_empty());
}

/// 别处来的音量照样要夹:1.7 这种数不该原样进播放器和设置文件。
#[test]
fn a_remote_volume_out_of_range_is_clamped() {
    let (ui, deck) = deck_window_pumped();
    wire(&ui, &deck);

    crate::sync::group::apply_remote_volume(&ui, 1.7);

    assert!(
        (ui.global::<Player>().get_volume() - 1.0).abs()
            < 1e-6
    );
}

/// 输出设备那一排:正在出声、报过音量的那台出它自己的音量条;不出声的不出。
/// 拖那条音量条发给那一台,条先跟手(#151)。
#[test]
fn each_sounding_device_shows_and_takes_its_own_volume() {
    use slint::Model as _;

    let (ui, deck) = deck_window_pumped();
    wire(&ui, &deck);
    crate::sync::group::bind(&ui, &deck.group);
    ui.global::<Shell>().set_devices(slint::ModelRc::new(
        slint::VecModel::from(
            ["pc", "tab"]
                .map(|id| crate::DeviceRow {
                    id: id.into(),
                    name: id.into(),
                    ..Default::default()
                })
                .to_vec(),
        ),
    ));
    deck.group.assume(Some(state(&["me", "pc"], true)));
    for (from, volume) in [("pc", 0.4), ("tab", 0.9)] {
        deck.group.handle(&syncplay::Event::DeviceReport {
            from: from.to_owned(),
            report: app_core::DeviceReportDto {
                entry_id: Some(12),
                fault: None,
                route: None,
                volume: Some(volume),
            },
        });
    }
    deck.group.paint_now(&ui);
    let row = |index| {
        ui.global::<Shell>()
            .get_devices()
            .row_data(index)
            .expect("那一行不见了")
    };

    assert!(row(0).has_volume);
    assert!((row(0).volume - 0.4).abs() < 1e-6);
    assert!(!row(1).has_volume, "不出声的那台不该有音量条");

    ui.global::<Shell>()
        .invoke_set_device_volume("pc".into(), 0.25);

    assert_eq!(
        deck.group.intents(),
        vec!["volume pc 0.25"]
    );
    assert!((row(0).volume - 0.25).abs() < 1e-6);
}

/// 出声设备备的是全局状态预告的下一首,不是本机队列里的下一首(#154):切歌时直接用,
/// 不必现取直链、现开流。只当遥控器的不出声,什么都不备;独奏照旧备本机队列的下一首。
#[test]
fn an_output_prefetches_the_track_the_group_announced() {
    let (_ui, deck) = deck_window();
    deck.queue.borrow_mut().replace(
        vec![track_with_id("a"), track_with_id("b")],
        0,
    );
    let id =
        |deck: &Deck| upcoming(deck).map(|track| track.id);

    assert_eq!(
        id(&deck),
        Some("b".to_owned()),
        "独奏备本机队列的下一首"
    );

    let mut announced = state(&["me"], true);
    announced.now.as_mut().expect("在放").next =
        Some(app_core::NextEntryDto {
            entry_id: 13,
            track: track_with_id("y"),
            at_us: 0,
        });
    deck.group.assume(Some(announced.clone()));
    assert_eq!(id(&deck), Some("y".to_owned()));

    announced.outputs = vec!["pc".to_owned()];
    announced.version += 1;
    deck.group.assume(Some(announced));
    assert_eq!(id(&deck), None, "只当遥控器不备");
}

/// 出声设备备好了此刻那一首就报一次就绪,同一版不重报;新的一版再报;只当遥控器不报(#154)。
#[test]
fn an_output_reports_ready_once_per_version() {
    let (ui, deck) = deck_window();
    let mut pending = state(&["me"], true);
    deck.group.assume(Some(pending.clone()));

    deck.group.ready(&ui, 12);
    deck.group.ready(&ui, 12);
    deck.group.ready(&ui, 99);
    pending.version += 1;
    deck.group.assume(Some(pending.clone()));
    deck.group.ready(&ui, 12);

    assert_eq!(
        deck.group.intents(),
        vec!["ready 12", "ready 12"],
        "每版一次;不是此刻那一条不报"
    );

    pending.version += 1;
    pending.outputs = vec!["pc".to_owned()];
    deck.group.assume(Some(pending));
    deck.group.ready(&ui, 12);
    assert_eq!(
        deck.group.intents().len(),
        2,
        "只当遥控器不报"
    );
}

/// 组此刻放的是组队列第 `revision` 版;`last` 是在放它的最后一首(没有预告的下一首)。
/// 版本号跟着往上走:同一版号的状态本机不收。
fn radio_state(revision: i64, last: bool) -> GroupStateDto {
    let mut state = state(&["me", "pc"], true);
    state.version = (revision * 2 + i64::from(last)) as u64;
    let now = state.now.as_mut().expect("该在放");
    now.revision = revision;
    if !last {
        now.next = Some(app_core::NextEntryDto {
            entry_id: 13,
            track: track_with_id("y"),
            at_us: 0,
        });
    }
    state
}

/// 组里开电台(#165):起播走组意图,应答里那一版组队列归电台;放到最后一首就续。
#[test]
fn the_radio_in_a_group_owns_the_revision_it_started() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(radio_state(2, false)));

    super::super::radio::begin(
        &ui,
        &deck,
        api::RadioMode::Fm,
        batch_of(&deck, &["a", "b"]),
    );
    assert_eq!(deck.group.intents(), vec!["play 0"]);
    deck.group.assume(Some(radio_state(3, false)));
    deck.group.answer(Some((7, 3)));

    assert!(
        !super::super::radio::due(&deck, 0),
        "还有下一首,不续"
    );
    deck.group.assume(Some(radio_state(3, true)));
    assert!(
        super::super::radio::due(&deck, 0),
        "放到电台那一版的最后一首,该续"
    );
    assert!(
        deck.queue.borrow().current().is_none(),
        "组里的电台不在本机队列上放"
    );
}

/// 组里有人点了别的歌:组队列换了一版,电台不再往里续。
#[test]
fn another_pick_in_the_group_stops_the_radio() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(radio_state(2, false)));
    super::super::radio::begin(
        &ui,
        &deck,
        api::RadioMode::Fm,
        batch_of(&deck, &["a", "b"]),
    );
    deck.group.assume(Some(radio_state(3, true)));
    deck.group.answer(Some((7, 3)));
    assert!(super::super::radio::due(&deck, 0));

    deck.group.assume(Some(radio_state(4, true)));

    assert!(!super::super::radio::due(&deck, 0));
}

/// 起播的组意图没成:电台不接组队列里的任何一版。
#[test]
fn a_failed_radio_start_in_the_group_owns_nothing() {
    let (ui, deck) = deck_window();
    wire(&ui, &deck);
    deck.group.assume(Some(radio_state(2, true)));
    super::super::radio::begin(
        &ui,
        &deck,
        api::RadioMode::Fm,
        batch_of(&deck, &["a", "b"]),
    );

    deck.group.answer(None);

    assert!(!super::super::radio::due(&deck, 0));
}
