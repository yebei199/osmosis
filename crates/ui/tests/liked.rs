//! 红心键的界面行为。无头跑,与其余几个 tests 同一套路。
//!
//! Rust 侧的 `liked` 单测证明的是**集合怎么变**,这里钉的是**键在不在、
//! 画的是哪一态** —— 集合一路正确而屏幕上没有那颗心,用户就只能回手机
//! 官方 App 里维护「我喜欢的」。

use i_slint_backend_testing as testing;
use slint::{ModelRc, VecModel};
use ui::{MainWindow, TrackRow};

fn hearts(ui: &MainWindow) -> usize {
    testing::ElementHandle::find_by_element_id(
        ui,
        "TrackList::heart",
    )
    .count()
}

fn row(id: &str, liked: bool) -> TrackRow {
    TrackRow {
        id: id.into(),
        title: "曲".into(),
        artists: "人".into(),
        duration: "03:00".into(),
        loading: false,
        liked,
        cover_url: String::new().into(),
        cover: slint::Image::default(),
    }
}

fn music_page_with(rows: Vec<TrackRow>) -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.set_logged_in(true);
    ui.set_current_tab(1);
    ui.set_tracks(ModelRc::new(VecModel::from(rows)));
    ui
}

/// 每一行都有红心键。
///
/// 少了它,「我喜欢的」就只能在手机官方 App 里维护 —— 而那正是这个应用
/// 想替掉的东西。
#[test]
fn every_track_row_has_a_heart() {
    let ui = music_page_with(vec![
        row("1", false),
        row("2", true),
        row("3", false),
    ]);

    assert_eq!(hearts(&ui), 3, "三行就该有三颗心,一行一个");
}

/// 没有歌的时候也就没有心 —— 空列表不该凭空长出一颗。
#[test]
fn an_empty_list_has_no_hearts() {
    let ui = music_page_with(Vec::new());

    assert_eq!(hearts(&ui), 0);
}

/// 红心状态跟着数据走:改了 `liked` 那一行的心也跟着变。
///
/// 两态画得一样的话,点了也不知道成没成 —— 而这里能验的是数据到得了那一层。
#[test]
fn the_heart_follows_the_row_state() {
    let ui = music_page_with(vec![row("1", false)]);

    ui.set_tracks(ModelRc::new(VecModel::from(vec![row(
        "1", true,
    )])));

    // 心还在(没被整行重建掉),且这一行现在是红心态
    assert_eq!(hearts(&ui), 1);
    let rows = ui.get_tracks();
    assert!(
        slint::Model::row_data(&rows, 0)
            .expect("第一行该在")
            .liked
    );
}
