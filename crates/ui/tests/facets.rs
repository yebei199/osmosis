//! 分组条与堆头的界面行为(#160)。无头跑。
//!
//! 分堆与筛选的规则由 `app_core::facets` 的单测钉住,绑定由 `ui` 的
//! `music::tests` 钉住;这里钉的是**界面上摆出来的东西**:条在不在、
//! 堆头是不是没有红心键、筛空了说的是哪一句。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use slint::{ModelRc, SharedString, VecModel};
use ui::{
    FacetChip, MainWindow, Player, Session, Shell, TrackRow,
};

fn song(id: &str) -> TrackRow {
    TrackRow {
        id: id.into(),
        title: "曲".into(),
        artists: "人".into(),
        duration: "03:00".into(),
        ..Default::default()
    }
}

fn pile(label: &str) -> TrackRow {
    TrackRow {
        title: label.into(),
        duration: "2 首".into(),
        header: true,
        ..Default::default()
    }
}

/// 每日推荐分区,分组条开着。
fn daily_with(rows: Vec<TrackRow>) -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.window()
        .set_size(slint::LogicalSize::new(1200.0, 900.0));
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_compact(false);
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_music_section(0);
    let labels: Vec<SharedString> =
        ["不分组", "歌手", "专辑"]
            .into_iter()
            .map(Into::into)
            .collect();
    let player = ui.global::<Player>();
    player.set_grouping_labels(ModelRc::new(
        VecModel::from(labels),
    ));
    player.set_chips(ModelRc::new(VecModel::from(vec![
        FacetChip {
            text: "有歌词 2".into(),
            chosen: false,
        },
    ])));
    player.set_facets_enabled(true);
    player.set_tracks(ModelRc::new(VecModel::from(rows)));
    ui
}

fn count(ui: &MainWindow, id: &str) -> usize {
    testing::ElementHandle::find_by_element_id(ui, id)
        .count()
}

fn labelled(ui: &MainWindow, label: &str) -> usize {
    testing::ElementHandle::find_by_accessible_label(
        ui, label,
    )
    .count()
}

/// 分组条摆出每一种分组;筛选那一排默认收着。
#[test]
fn the_bar_offers_every_grouping() {
    let ui = daily_with(vec![song("1")]);

    assert_eq!(count(&ui, "FacetBar::grouping-pill"), 3);
    assert_eq!(count(&ui, "FacetBar::filter-toggle"), 1);
    assert_eq!(count(&ui, "FacetBar::chip-pill"), 0);
}

/// 选了 chip 时筛选那一排常驻,否则看不见筛了什么。
#[test]
fn chosen_chips_keep_the_filter_row_open() {
    let ui = daily_with(vec![song("1")]);
    ui.global::<Player>().set_chosen_count(1);

    assert_eq!(count(&ui, "FacetBar::chip-pill"), 1);
    assert_eq!(labelled(&ui, "筛选 · 1"), 1);
}

/// 堆头读得出堆名与首数,但没有红心键 —— 它不是一首歌。
#[test]
fn a_pile_header_has_no_heart() {
    let ui =
        daily_with(vec![pile("甲"), song("1"), song("2")]);

    assert_eq!(count(&ui, "TrackList::heart"), 2);
    assert_eq!(labelled(&ui, "甲 2 首"), 1);
}

/// 不是歌单类的视图(搜索、电台)不挂分组条。
#[test]
fn no_bar_when_the_view_has_no_facets() {
    let ui = daily_with(vec![song("1")]);
    ui.global::<Player>().set_facets_enabled(false);

    assert_eq!(count(&ui, "FacetBar::filter-toggle"), 0);
}

/// 筛空了说「没有符合筛选的歌」,不摆开局的空状态。
#[test]
fn an_empty_filter_says_so() {
    let ui = daily_with(Vec::new());
    ui.global::<Player>().set_chosen_count(1);

    assert_eq!(labelled(&ui, "没有符合筛选的歌"), 1);
    assert!(!ui.global::<Shell>().get_music_empty());
}
