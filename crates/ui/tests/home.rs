//! Home(壳的应用启动器)的界面行为。无头跑,与 controls.rs 同一套路。
//!
//! 瓦片的视觉断言不了,这里钉的是行为:瓦片是真按钮、点了真切页。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use ui::MainWindow;
use ui::Session;
use ui::Shell;

/// 登录后落在 Home。
fn home() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_current_tab(0);
    ui.global::<Shell>().set_compact(false);
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
        ui.global::<Shell>().get_current_tab(),
        1,
        "点音乐瓦片该切到音乐页"
    );
}

// 「迷你播放条只在手上有歌时出现」搬去了 player_bar.rs —— #81 起那颗胶囊
// 归 PlayerBar,而它现在管的是三页而不只是 Home,断言跟着条走。
