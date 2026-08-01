//! 控制条上进度条与音量的界面行为。无头跑,与 music_nav.rs 同一套路。
//!
//! `progress::ratio` 证明的是**算得对**,这些断言管的是**摆得对、拖得动**。

use i_slint_backend_testing as testing;
use ui::MainWindow;

fn present(ui: &MainWindow, id: &str) -> bool {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .is_some()
}

/// 登录之后才有控制条。宽版式,免得断言落到紧凑版那一份上。
fn wide_page() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.set_logged_in(true);
    ui.set_current_tab(1);
    ui.set_compact(false);
    ui
}

/// 音量滑块默认收起,点喇叭才展开。
///
/// 常驻滑块在紧凑版式里塞不下:那一排的最小宽度已经约 470px,
/// 而手机内容区只有 ~360px —— 那正是紧凑版拆两行的原因。
#[test]
fn the_volume_slider_starts_collapsed() {
    let ui = wide_page();

    assert!(
        present(&ui, "VolumeControl::speaker"),
        "喇叭键该一直在"
    );
    assert!(
        !present(&ui, "VolumeControl::slider"),
        "滑块默认不该展开"
    );

    let speaker =
        testing::ElementHandle::find_by_element_id(
            &ui,
            "VolumeControl::speaker",
        )
        .next()
        .expect("找不到喇叭键");
    speaker.invoke_accessible_default_action();

    assert!(
        present(&ui, "VolumeControl::slider"),
        "点了之后该展开"
    );
}

/// 进度条只在手上有歌时出现。
///
/// 没在放的时候摆一条空槽,会让人以为点它能从头开始 —— 而那时根本没有"头"。
#[test]
fn the_progress_bar_appears_only_with_a_track() {
    let ui = wide_page();

    ui.set_has_track(false);
    assert!(!present(&ui, "MainWindow::wide-progress"));

    ui.set_has_track(true);
    assert!(present(&ui, "MainWindow::wide-progress"));
}

// 「拖动时不被每秒的位置刷新拽回去」这一条**没有测试**。
//
// 骨架里原本有它,写不出来:无头测试驱动不了真实指针,而 ProgressBar 既不是
// 独立编译的类型(build.rs 只编 app.slint)、又长在一个 `if` 里,Rust 侧
// 引用不到它的属性。造一条只断言"属性没被改写"的测试等于自欺,故不留。
//
// 那一行因此写成声明式的 `dragging ? drag-ratio : ratio`(见 controls.slint),
// 靠形状而不是靠断言;改它要手动验一次:拖住不放,看滑块会不会自己往回跳。
