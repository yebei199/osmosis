//! 设置页的界面行为。无头跑,与 controls.rs 同一套路。
//!
//! 主题三档与退出登录都是「喊一声,值由 Rust 写回」:控件自己不置位,
//! 这里钉的就是喊没喊、喊的是哪一档。

use i_slint_backend_testing as testing;
use slint::ComponentHandle;
use ui::MainWindow;

fn settings_page() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.set_logged_in(true);
    // 设置页是 tab 3(个人主页插在 2)。
    ui.set_current_tab(3);
    ui.set_compact(false);
    ui
}

fn invoke(ui: &MainWindow, id: &str) {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .unwrap_or_else(|| panic!("找不到 {id}"))
        .invoke_accessible_default_action();
}

/// **日月开关是主题区的主视觉**(#68,从控制簇迁来):拨一下报与当前
/// 相反的显式档(0 深 1 浅),不自己置位 —— 档位真相在 api::settings。
#[test]
fn the_theme_switch_asks_for_the_opposite_mode() {
    let ui = settings_page();

    let asked =
        std::rc::Rc::new(std::cell::Cell::new(-1i32));
    let seen = asked.clone();
    ui.on_theme_mode_selected(move |index| {
        seen.set(index);
    });

    let dark = ui.global::<ui::Theme>().get_dark();
    // 开关的 a11y 动作在其内部 TouchArea 上;控制簇那颗已退场,
    // 全应用只剩设置页这一颗,按组件内元素 id 找不会歧义。
    invoke(&ui, "DayNightSwitch::touch");
    assert_eq!(
        asked.get(),
        if dark { 1 } else { 0 },
        "拨开关该报与当前相反的显式档"
    );
}

/// 「跟随系统」单列一项:开 → 报 2;跟随中再拨 → 报当前生效的显式档,
/// 外观不跳变。
#[test]
fn the_follow_system_toggle_reports_mode_two() {
    let ui = settings_page();

    let asked =
        std::rc::Rc::new(std::cell::Cell::new(-1i32));
    let seen = asked.clone();
    ui.on_theme_mode_selected(move |index| {
        seen.set(index);
    });

    let theme = ui.global::<ui::Theme>();
    theme.set_mode(0);
    invoke(&ui, "SettingsPage::follow-toggle");
    assert_eq!(asked.get(), 2, "没在跟随时拨它该报 2");

    theme.set_mode(2);
    let dark = theme.get_dark();
    invoke(&ui, "SettingsPage::follow-toggle");
    assert_eq!(
        asked.get(),
        if dark { 0 } else { 1 },
        "跟随中拨它该落回当前生效的显式档"
    );
}

/// 动态极光按钮的开关也只喊一声,值由 Rust 写回(aurora_btn.rs)。
#[test]
fn the_aurora_toggle_asks_without_flipping() {
    let ui = settings_page();
    ui.set_aurora_buttons_on(true);

    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = asked.clone();
    ui.on_aurora_buttons_toggled(move || {
        counter.set(counter.get() + 1);
    });

    invoke(&ui, "SettingsPage::aurora-toggle");

    assert_eq!(asked.get(), 1, "拨一下该喊一声");
    assert!(
        ui.get_aurora_buttons_on(),
        "值该纹丝不动 —— 写它是 Rust 的活"
    );
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
