//! Music 页二级导航的界面行为。无头跑,与 banner.rs / login.rs 同一套路。
//!
//! 这里钉的是「哪一块在树里」——`Section::from_index` 证明的是**该拉什么**,
//! 而这些 `if` 决定的是**用户看到什么**。后者写错的话,数据一路正确而屏幕上
//! 摆的是另一个分区。

use i_slint_backend_testing as testing;
use ui::MainWindow;

fn present(ui: &MainWindow, id: &str) -> bool {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .is_some()
}

/// 登录之后才能看到 Music 页 —— 未登录时整块内容区都不建。
fn music_page() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.set_logged_in(true);
    ui.set_current_tab(1);
    ui
}

/// 选中的那一节在树里,别的分区不在。
///
/// 用 `if` 而不是 `visible`:不在树上才不会被 tab 键走到,也才测得准。
#[test]
fn only_the_selected_section_is_in_the_tree() {
    let ui = music_page();

    // 0 = 每日推荐:摆曲目列表,没有搜索框
    ui.set_music_section(0);
    assert!(present(&ui, "MainWindow::track-list"));
    assert!(!present(&ui, "MainWindow::keyword"));
    assert!(!present(&ui, "MainWindow::playlist-list"));

    // 1 = 我的歌单:摆歌单列表,不摆曲目列表。
    //
    // 这里曾经断言的是一个占位文本 —— 歌单列表做出来之后,这一节摆的东西换了,
    // 但「摆自己的东西、不摆一批歌」这条没变。
    ui.set_music_section(1);
    assert!(present(&ui, "MainWindow::playlist-list"));
    assert!(
        !present(&ui, "MainWindow::track-list"),
        "歌单分区不该摆曲目列表 —— 那是别的分区的一批歌"
    );

    // 2 = 搜索:搜索框出现
    ui.set_music_section(2);
    assert!(present(&ui, "MainWindow::keyword"));
    assert!(present(&ui, "MainWindow::track-list"));
}

/// 竖栏只在宽版式、分段条只在紧凑版式。
///
/// 两个都在的话,窄窗口上会同时占掉左边和顶部两块地方。
///
/// 直接给定版式而不是改窗口尺寸:无头后端里 `set_size` 不驱动 `width`,
/// 版式因此推不出来。这条证明的是「给定版式该摆哪个导航」,
/// 「宽度推出版式」那半由 app.slint 里那两行绑定自己保证。
#[test]
fn the_rail_is_wide_only_and_the_bar_is_compact_only() {
    let ui = music_page();

    ui.set_compact(false);
    assert!(present(&ui, "MainWindow::music-rail"));
    assert!(!present(&ui, "MainWindow::music-bar"));

    ui.set_compact(true);
    assert!(present(&ui, "MainWindow::music-bar"));
    assert!(
        !present(&ui, "MainWindow::music-rail"),
        "紧凑版式下竖栏会把内容挤没"
    );
}

/// 收起竖栏不改变当前在看哪一节 —— 收起是「少占地方」,不是「回到默认页」。
#[test]
fn collapsing_the_rail_keeps_the_selection() {
    let ui = music_page();
    ui.set_music_section(3);

    ui.set_rail_collapsed(true);

    assert_eq!(ui.get_music_section(), 3);
    assert!(present(&ui, "MainWindow::track-list"));
}

/// 切换分区不动正在播的那首。
///
/// 这一层换的是列表,不是播放器 —— 做错的现象是点一下侧栏歌就停了。
#[test]
fn switching_sections_does_not_disturb_playback() {
    let ui = music_page();
    ui.set_now_title("紅蓮華".into());
    ui.set_is_playing(true);

    ui.set_music_section(1);
    ui.set_music_section(3);

    assert_eq!(ui.get_now_title(), "紅蓮華");
    assert!(ui.get_is_playing(), "换个列表看不该把歌停掉");
}
