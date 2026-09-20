//! 个人主页的界面行为。无头跑,与 controls.rs 同一套路。
//!
//! 统计数字是 Rust 推的字符串,视觉断言不了;这里钉的是取数的时机与
//! 「没数不摆卡」。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use ui::MainWindow;
use ui::Player;
use ui::Profile;
use ui::Session;
use ui::Shell;

fn present(ui: &MainWindow, id: &str) -> bool {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .is_some()
}

fn window() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_compact(false);
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

    ui.global::<Shell>().set_current_tab(2);
    // 无头下条件元素惰性实例化,init 在第一次元素查询时才触发;
    // 查一下页面里的任意元素,把实例化逼出来。
    let _ = present(&ui, "ProfilePage::stats-row");

    assert_eq!(asked.get(), 1, "进页该喊一声");
}

/// 统计没回来之前不摆卡:空数字的卡是摆设(docs/design.md 硬规则 8)。
#[test]
fn stat_cards_wait_for_the_data() {
    let ui = window();
    ui.global::<Shell>().set_current_tab(2);

    ui.global::<Profile>().set_loaded(false);
    assert!(!present(&ui, "ProfilePage::stats-row"));

    ui.global::<Profile>().set_loaded(true);
    assert!(present(&ui, "ProfilePage::stats-row"));
}

/// 网易云一节:没绑摆码,绑了摆昵称。
///
/// 两个方向都要钉。只钉「没绑摆码」的话,一个永远摆着码的页面照样通过 ——
/// 而那意味着绑好之后用户还在对着一张已经没用的码。
#[test]
fn the_netease_section_swaps_the_code_for_the_nickname() {
    let ui = window();
    ui.global::<Shell>().set_current_tab(2);

    assert!(
        present(&ui, "ProfilePage::netease-code"),
        "没绑的时候该摆出二维码"
    );
    assert!(
        !present(&ui, "ProfilePage::netease-name"),
        "没绑哪来的昵称"
    );

    ui.global::<Profile>().set_netease_bound(true);
    ui.global::<Profile>()
        .set_netease_nickname("某人".into());

    assert!(
        present(&ui, "ProfilePage::netease-name"),
        "绑上了该报昵称"
    );
    assert!(
        !present(&ui, "ProfilePage::netease-code"),
        "绑上了就别再摆码 —— 那张码已经没用了"
    );
}

/// 「解绑」真的接到了 Rust 那一侧。
///
/// 接空了的现象是按下去什么都不发生,而那与「请求发出去了但失败了」
/// 在屏幕上长得一模一样。
#[test]
fn the_unbind_button_reaches_rust() {
    let ui = window();
    ui.global::<Shell>().set_current_tab(2);
    ui.global::<Profile>().set_netease_bound(true);

    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = asked.clone();
    ui.global::<Profile>().on_unbind_netease(move || {
        counter.set(counter.get() + 1);
    });

    testing::ElementHandle::find_by_accessible_label(
        &ui, "解绑",
    )
    .next()
    .expect("绑上了该有一颗解绑键")
    .invoke_accessible_default_action();

    assert_eq!(asked.get(), 1, "按一下该发一次解绑");
}

/// 没放过歌也能选输出设备。
///
/// 控制条挂在 `Player.has-track` 上,抽屉跟着它一起不存在 —— 冷启动时
/// 「点歌之前先选被控设备」这条产品规则在界面上无路可走(#102 之五)。
/// 个人主页那张名册卡因此摆同一个选择器。
#[test]
fn the_output_device_can_be_picked_without_a_track() {
    let ui = window();
    ui.global::<Player>().set_has_track(false);
    ui.global::<Shell>().set_current_tab(2);

    assert!(
        present(&ui, "ProfilePage::output-strip"),
        "没曲目时也得有一条选输出设备的路"
    );

    // 哨兵值:回调没被叫到时留在这里,否则空串与「选回本机」分不开。
    let picked = std::rc::Rc::new(std::cell::RefCell::new(
        "没点过".to_owned(),
    ));
    let sink = picked.clone();
    ui.global::<Shell>().on_set_output(move |id| {
        *sink.borrow_mut() = id.to_string();
    });

    testing::ElementHandle::find_by_accessible_label(
        &ui, "输出到 本机",
    )
    .next()
    .expect("本机那颗芯片该在")
    .invoke_accessible_default_action();

    assert_eq!(
        picked.borrow().as_str(),
        "",
        "点本机就是把输出选回本机,id 是空串"
    );
}
