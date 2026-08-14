//! 曲目行封面槽位的界面行为。无头跑,与其余几个 tests 同一套路。
//!
//! Rust 侧的 `thumbnail` 单测证明的是**图怎么缓存、怎么淘汰**,这里钉的是
//! **行上有没有那块地方** —— 缓存一路正确而行里没有槽位,列表就还是纯文字,
//! 而那正是这次要改掉的东西。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use slint::{ModelRc, VecModel};
use ui::Session;
use ui::{MainWindow, TrackRow};

fn covers(ui: &MainWindow) -> usize {
    testing::ElementHandle::find_by_element_id(
        ui,
        "TrackList::cover-slot",
    )
    .count()
}

fn row(id: &str) -> TrackRow {
    TrackRow {
        id: id.into(),
        title: "曲".into(),
        artists: "人".into(),
        duration: "03:00".into(),
        loading: false,
        liked: false,
        cover_url: format!("https://cdn/{id}.jpg").into(),
        cover: slint::Image::default(),
    }
}

fn music_page_with(rows: Vec<TrackRow>) -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.set_current_tab(1);
    ui.set_tracks(ModelRc::new(VecModel::from(rows)));
    ui
}

/// 每一行都有封面槽位。
///
/// 列表现在是虚拟化的(ListView),所以这条同时在钉「可见的那几行确实被实例化
/// 了」—— 槽位数为 0 既可能是忘了画,也可能是整列压根没渲染。
#[test]
fn every_track_row_has_a_cover_slot() {
    let ui =
        music_page_with(vec![row("1"), row("2"), row("3")]);

    assert_eq!(
        covers(&ui),
        3,
        "三行就该有三个封面槽位,一行一个"
    );
}

/// 没有歌的时候也就没有槽位 —— 空列表不该凭空长出一块占位色。
#[test]
fn an_empty_list_has_no_cover_slot() {
    let ui = music_page_with(Vec::new());

    assert_eq!(covers(&ui), 0);
}

/// 长列表只实例化看得见的那一段。
///
/// 这是整件事的前提:每行配一张封面之后,全量实例化意味着上千张图同时在内存里
/// (一张 500×500 的原图解出来 1MB)。这条一旦变红,说明列表退回了全量渲染 ——
/// 而那不会报错,只会让内存悄悄涨上去。
#[test]
fn a_long_list_only_renders_what_fits() {
    let many: Vec<TrackRow> =
        (0..500).map(|i| row(&i.to_string())).collect();
    let ui = music_page_with(many);

    let rendered = covers(&ui);
    assert!(rendered > 0, "可见的那几行必须画出来");
    assert!(
        rendered < 500,
        "五百行全画出来了({rendered}),列表没有虚拟化"
    );
}
