//! Home(壳的应用启动器)的界面行为。无头跑,与 controls.rs 同一套路。
//!
//! 瓦片与迷你播放条的视觉断言不了,这里钉的是行为与"摆没摆":
//! 瓦片是真按钮、点了真切页;迷你条只在手上有歌时出现(docs/adr/0024)。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use ui::MainWindow;
use ui::Session;

fn present(ui: &MainWindow, id: &str) -> bool {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .is_some()
}

/// 登录后落在 Home。
fn home() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.set_current_tab(0);
    ui.set_compact(false);
    ui
}

/// 音乐瓦片是入口:点它切到音乐页。
#[test]
fn the_music_tile_opens_the_music_app() {
    let ui = home();

    // 按元素 id 找而不是按标签:瓦片里那行「音乐」文字的无障碍标签
    // 与按钮同名,按标签找会先撞上文字。
    testing::ElementHandle::find_by_element_id(
        &ui,
        "MainWindow::music-tile",
    )
    .next()
    .expect("找不到音乐瓦片")
    .invoke_accessible_default_action();

    assert_eq!(
        ui.get_current_tab(),
        1,
        "点音乐瓦片该切到音乐页"
    );
}

/// 迷你播放条跨应用常驻,但只在手上有歌时出现 —— 没歌时一枚空胶囊
/// 只是骗人的摆设(docs/design.md 硬规则 8)。
#[test]
fn the_mini_player_appears_only_with_a_track() {
    let ui = home();

    ui.set_has_track(false);
    assert!(!present(&ui, "MainWindow::home-mini"));

    ui.set_has_track(true);
    assert!(present(&ui, "MainWindow::home-mini"));
}
