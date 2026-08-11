//! 设置页的界面行为。无头跑,与 controls.rs 同一套路。
//!
//! 主题三档与退出登录都是「喊一声,值由 Rust 写回」:控件自己不置位,
//! 这里钉的就是喊没喊、喊的是哪一档。

use i_slint_backend_testing as testing;
use ui::MainWindow;

fn settings_page() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.set_logged_in(true);
    ui.set_current_tab(2);
    ui.set_compact(false);
    ui
}

fn invoke(ui: &MainWindow, id: &str) {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .unwrap_or_else(|| panic!("找不到 {id}"))
        .invoke_accessible_default_action();
}

/// 三档分段控件:点哪档报哪档的序号(0 深 1 浅 2 跟随系统),不自己置位。
#[test]
fn the_theme_segments_ask_for_the_matching_mode() {
    let ui = settings_page();

    let asked =
        std::rc::Rc::new(std::cell::Cell::new(-1i32));
    let seen = asked.clone();
    ui.on_theme_mode_selected(move |index| {
        seen.set(index);
    });

    invoke(&ui, "SettingsPage::theme-light");
    assert_eq!(asked.get(), 1, "浅色档该报 1");

    invoke(&ui, "SettingsPage::theme-system");
    assert_eq!(asked.get(), 2, "跟随系统档该报 2");

    invoke(&ui, "SettingsPage::theme-dark");
    assert_eq!(asked.get(), 0, "深色档该报 0");
}

/// 退出登录只喊一声,登录态由 Rust 写回 —— 控件不自己清会话。
#[test]
fn logging_out_asks_without_flipping_the_state() {
    let ui = settings_page();

    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = asked.clone();
    ui.on_logout(move || {
        counter.set(counter.get() + 1);
    });

    invoke(&ui, "SettingsPage::logout-button");

    assert_eq!(asked.get(), 1, "按一下该喊一声");
    assert!(
        ui.get_logged_in(),
        "登录态该纹丝不动 —— 清会话是 Rust 的活"
    );
}
