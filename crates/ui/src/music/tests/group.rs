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
