//! 歌单列表与详情的界面行为。无头跑,与 login.rs / music_nav.rs 同一套路。
//!
//! 这里钉的是**两层之间的关系**:Rust 侧的 `Source` 证明的是「该问谁要曲目」,
//! 而这些 `if` 决定的是「此刻用户在哪一层」。后者写错的话,返回键该收哪一层
//! 就说不清了。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use ui::MainWindow;
use ui::Session;

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
    ui.set_current_tab(1);
    ui.set_music_section(1);
    ui
}

/// 列表与详情互斥,任何时刻只有一层在。
///
/// 两个都在的话,返回键该收哪一层就说不清了。
#[test]
fn the_list_and_the_detail_are_never_both_shown() {
    let ui = playlists_section();

    // 没打开任何歌单 = 停在列表层
    assert!(present(&ui, "MainWindow::playlist-list"));
    assert!(!present(&ui, "MainWindow::playlist-header"));

    ui.set_open_playlist_name("睡前".into());
    assert!(present(&ui, "MainWindow::playlist-header"));
    assert!(
        !present(&ui, "MainWindow::playlist-list"),
        "进了详情就不该还摆着列表"
    );
}

/// 进详情后标题是那个歌单的名字,不是「我的歌单」。
#[test]
fn opening_a_playlist_shows_its_name() {
    let ui = playlists_section();
    ui.set_open_playlist_name("华语经典".into());

    assert_eq!(ui.get_open_playlist_name(), "华语经典");
    assert!(present(&ui, "MainWindow::playlist-header"));
    // 详情里摆的是曲目,与别的分区同一个列表组件
    assert!(present(&ui, "MainWindow::track-list"));
}

/// 返回回到列表,且**留在歌单分区** —— 不是跳回每日推荐。
#[test]
fn going_back_returns_to_the_list() {
    let ui = playlists_section();
    ui.set_open_playlist_name("睡前".into());

    ui.set_open_playlist_name("".into());

    assert_eq!(
        ui.get_music_section(),
        1,
        "返回是退一层,不是换一节"
    );
    assert!(present(&ui, "MainWindow::playlist-list"));
    assert!(!present(&ui, "MainWindow::playlist-header"));
}

/// 歌单分区停在列表层时不摆曲目列表 —— 那一层摆的是歌单。
#[test]
fn the_list_layer_shows_playlists_not_tracks() {
    let ui = playlists_section();

    assert!(
        !present(&ui, "MainWindow::track-list"),
        "列表层摆的是歌单,不是某一批歌"
    );
}
