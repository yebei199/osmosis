//! 个人主页的界面行为。无头跑,与 controls.rs 同一套路。
//!
//! 统计数字是 Rust 推的字符串,视觉断言不了;这里钉的是取数的时机与
//! 「没数不摆卡」。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use ui::MainWindow;
use ui::Profile;
use ui::Session;

fn present(ui: &MainWindow, id: &str) -> bool {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .is_some()
}

fn window() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.set_compact(false);
    ui
}

/// 进个人主页就喊 profile-shown:取不取、取几次的判断在 Rust 侧。
#[test]
fn entering_the_profile_asks_for_stats() {
    let ui = window();

    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = asked.clone();
    ui.global::<Profile>().on_shown(move || {
        counter.set(counter.get() + 1);
    });

    ui.set_current_tab(2);
    // 无头下条件元素惰性实例化,init 在第一次元素查询时才触发;
    // 查一下页面里的任意元素,把实例化逼出来。
    let _ = present(&ui, "ProfilePage::stats-row");

    assert_eq!(asked.get(), 1, "进页该喊一声");
}

/// 统计没回来之前不摆卡:空数字的卡是摆设(docs/design.md 硬规则 8)。
#[test]
fn stat_cards_wait_for_the_data() {
    let ui = window();
    ui.set_current_tab(2);

    ui.global::<Profile>().set_loaded(false);
    assert!(!present(&ui, "ProfilePage::stats-row"));

    ui.global::<Profile>().set_loaded(true);
    assert!(present(&ui, "ProfilePage::stats-row"));
}
