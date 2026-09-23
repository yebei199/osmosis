//! 歌单列表与详情的界面行为。无头跑,与 login.rs / music_nav.rs 同一套路。
//!
//! 这里钉的是**两层之间的关系**:Rust 侧的 `Source` 证明的是「该问谁要曲目」,
//! 而这些 `if` 决定的是「此刻用户在哪一层」。后者写错的话,返回键该收哪一层
//! 就说不清了。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use ui::Library;
use ui::MainWindow;
use ui::Player;
use ui::Session;
use ui::Shell;

fn present(ui: &MainWindow, id: &str) -> bool {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .is_some()
}

/// 停在「我的歌单」这一节。
fn playlists_section() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_music_section(1);
    ui
}

/// 列表与详情互斥,任何时刻只有一层在。
///
/// 两个都在的话,返回键该收哪一层就说不清了。
#[test]
fn the_list_and_the_detail_are_never_both_shown() {
    let ui = playlists_section();

    // 没打开任何歌单 = 停在列表层
    assert!(present(&ui, "MusicPage::playlist-list"));
    assert!(!present(&ui, "MusicPage::playlist-header"));

    ui.global::<Library>()
        .set_open_playlist_name("睡前".into());
    assert!(present(&ui, "MusicPage::playlist-header"));
    assert!(
        !present(&ui, "MusicPage::playlist-list"),
        "进了详情就不该还摆着列表"
    );
}

/// 进详情后标题是那个歌单的名字,不是「我的歌单」。
#[test]
fn opening_a_playlist_shows_its_name() {
    let ui = playlists_section();
    ui.global::<Library>()
        .set_open_playlist_name("华语经典".into());

    assert_eq!(
        ui.global::<Library>().get_open_playlist_name(),
        "华语经典"
    );
    assert!(present(&ui, "MusicPage::playlist-header"));
    // 详情里摆的是曲目,与别的分区同一个列表组件。曲目到了才摆列表,
    // 一首都没有时是空状态(#116)。
    ui.global::<Player>().set_tracks(slint::ModelRc::new(
        slint::VecModel::from(
            vec![ui::TrackRow::default()],
        ),
    ));
    assert!(present(&ui, "MusicPage::track-list"));
}

/// 返回回到列表,且**留在歌单分区** —— 不是跳回每日推荐。
#[test]
fn going_back_returns_to_the_list() {
    let ui = playlists_section();
    ui.global::<Library>()
        .set_open_playlist_name("睡前".into());

    ui.global::<Library>()
        .set_open_playlist_name("".into());

    assert_eq!(
        ui.global::<Shell>().get_music_section(),
        1,
        "返回是退一层,不是换一节"
    );
    assert!(present(&ui, "MusicPage::playlist-list"));
    assert!(!present(&ui, "MusicPage::playlist-header"));
}

/// 歌单分区停在列表层时不摆曲目列表 —— 那一层摆的是歌单。
#[test]
fn the_list_layer_shows_playlists_not_tracks() {
    let ui = playlists_section();

    assert!(
        !present(&ui, "MusicPage::track-list"),
        "列表层摆的是歌单,不是某一批歌"
    );
}

// ── #116:歌单页下半部不许空着 ──

/// 这一格在页面里的底边,离页面底还有多远。
fn blank_below(ui: &MainWindow, id: &str) -> f32 {
    let el =
        testing::ElementHandle::find_by_element_id(ui, id)
            .next()
            .unwrap_or_else(|| panic!("{id} 该在"));
    let page =
        testing::ElementHandle::find_by_element_type_name(
            ui,
            "MusicPage",
        )
        .next()
        .expect("音乐页该在");
    let bottom = |h: &testing::ElementHandle| {
        h.absolute_position().y + h.size().height
    };
    bottom(&page) - bottom(&el)
}

/// 页边距 16 加上一格布局间距 12:没有控制条时,列表底下最多空这么多。
const BARE_BLANK: f32 = 16.0 + 12.0;
/// `BarMetrics.page-reserve`:条身 62 + 上下两段 22。
const BAR_RESERVE: f32 = 62.0 + 22.0 * 2.0;

/// 没有控制条时,歌单列表一直铺到页底 —— 页尾不再垫一块写死的 100px。
#[test]
fn the_playlist_list_reaches_the_bottom_without_a_bar() {
    let ui = playlists_section();
    ui.global::<Player>().set_has_track(false);

    let blank =
        blank_below(&ui, "MusicPage::playlist-list");
    assert!(
        blank <= BARE_BLANK,
        "没有控制条,列表底下却空着 {blank}px"
    );
}

/// 控制条在时,底部恰好让出它那一块,不多也不少。
#[test]
fn the_playlist_list_leaves_exactly_the_bar_reserve() {
    let ui = playlists_section();
    ui.global::<Player>().set_has_track(true);

    let blank =
        blank_below(&ui, "MusicPage::playlist-list");
    assert!(
        (BAR_RESERVE..=BAR_RESERVE + 12.0).contains(&blank),
        "控制条要让出 {BAR_RESERVE}px,实际空着 {blank}px"
    );
}

/// 歌单详情还没有曲目时,空状态独占这一层 —— 不再和一张空列表各分一半。
#[test]
fn an_empty_detail_is_all_empty_state() {
    let ui = playlists_section();
    ui.global::<Player>().set_has_track(false);
    ui.global::<Library>()
        .set_open_playlist_name("睡前".into());

    assert!(
        !present(&ui, "MusicPage::track-list"),
        "一首都没有时摆一张空列表,下半页就是空的"
    );
    let blank = blank_below(&ui, "MusicPage::empty-state");
    assert!(
        blank <= BARE_BLANK,
        "空状态该铺到页底,底下却空着 {blank}px"
    );
}

/// 曲目到了,列表接手整层,空状态退场。
#[test]
fn a_filled_detail_is_all_track_list() {
    let ui = playlists_section();
    ui.global::<Player>().set_has_track(false);
    ui.global::<Library>()
        .set_open_playlist_name("睡前".into());
    ui.global::<Player>().set_tracks(slint::ModelRc::new(
        slint::VecModel::from(
            vec![ui::TrackRow::default()],
        ),
    ));

    assert!(!present(&ui, "MusicPage::empty-state"));
    let blank = blank_below(&ui, "MusicPage::track-list");
    assert!(
        blank <= BARE_BLANK,
        "曲目列表该铺到页底,底下却空着 {blank}px"
    );
}
