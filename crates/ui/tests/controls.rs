//! 控制条上那几颗键的界面行为:拨得动、拨完喊得对、状态报得出。无头跑。
//!
//! `progress::ratio` 证明的是**算得对**,这些断言管的是**拨得动**。
//! 条身本身摆得对不对(在哪几页、翻不翻得动、尺寸回写)归 player_bar.rs ——
//! #81 把条收成一个组件之后,那些断言跟着搬去了条自己的测试里。

use i_slint_backend_testing as testing;
use slint::ComponentHandle;
use ui::MainWindow;
use ui::Player;
use ui::Session;
use ui::Shell;

fn present(ui: &MainWindow, id: &str) -> bool {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .is_some()
}

/// 控制条上那颗随机键。
///
/// 按无障碍标签找而不是按元素 id:那一排四颗键都是同一个 `RoundControl`,
/// 元素 id 分不开它们,而标签本来就是为了让人分得开才有的。
fn shuffle_key(ui: &MainWindow) -> testing::ElementHandle {
    modes_face(ui);
    testing::ElementHandle::find_by_accessible_label(
        ui,
        "随机播放",
    )
    .next()
    .expect("找不到随机键")
}

/// 登录、有歌、宽版式。#81 起整条只在手上有歌时才摆 —— 没歌时连条身都不在,
/// 于是这些断言全要先有一首歌垫着。
fn wide_page() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_compact(false);
    ui.global::<Player>().set_has_track(true);
    ui
}

/// 随机、循环与音量翻到面 B 上了(#81):三面各摆一簇,
/// 拨它们之前先把条翻过去。
fn modes_face(ui: &MainWindow) {
    ui.global::<Shell>().set_bar_face(1);
}

/// 音量滑块默认收起,点喇叭才展开。
///
/// 常驻滑块在紧凑版式里塞不下:那一排的最小宽度已经约 470px,
/// 而手机内容区只有 ~360px —— 那正是紧凑版拆两行的原因。
#[test]
fn the_volume_slider_starts_collapsed() {
    let ui = wide_page();
    modes_face(&ui);

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


/// **随机播放有自己的键了。**
///
/// 在此之前它借的是日月开关 —— 一个画着太阳和月亮的控件,谁看都以为它管明暗,
/// 于是播放顺序在界面上等于没有控件。新键拨一下只喊一声,值仍由 Rust 写回来
/// (与之前同一条规矩,只是换了个控件承担)。
#[test]
fn the_shuffle_button_asks_without_setting_the_property() {
    let ui = wide_page();
    ui.global::<Player>().set_shuffle_on(false);

    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = asked.clone();
    ui.global::<Player>().on_shuffle_toggled(move || {
        counter.set(counter.get() + 1);
    });

    shuffle_key(&ui).invoke_accessible_default_action();

    assert_eq!(asked.get(), 1, "拨一下该喊一声");
    assert!(
        !ui.global::<Player>().get_shuffle_on(),
        "值该纹丝不动 —— 写它是 Rust 的活,不是控件的"
    );
}

/// 新键要看得出随机开没开。
///
/// 颜色断言不了,但它对外报的 checked 状态可以 —— 那也正是读屏软件念的那一位。
#[test]
fn the_shuffle_button_shows_whether_shuffle_is_on() {
    let ui = wide_page();

    ui.global::<Player>().set_shuffle_on(false);
    assert_eq!(
        shuffle_key(&ui).accessible_checked(),
        Some(false)
    );

    ui.global::<Player>().set_shuffle_on(true);
    assert_eq!(
        shuffle_key(&ui).accessible_checked(),
        Some(true),
        "开着就该报开着 —— 读屏软件念的是这一位"
    );
}

/// 控制簇上那颗循环键,按它此刻的标签找 —— 标签随三态换,
/// 读屏念的就是它(checked 只说得出开没开,说不出列表还是单曲)。
fn loop_key(
    ui: &MainWindow,
    label: &str,
) -> Option<testing::ElementHandle> {
    modes_face(ui);
    testing::ElementHandle::find_by_accessible_label(
        ui, label,
    )
    .next()
}

/// 循环键拨一下只喊一声,值由 Rust 写回 —— 与随机键同一条规矩。
#[test]
fn the_loop_button_asks_without_setting_the_property() {
    let ui = wide_page();
    ui.global::<Player>().set_loop_mode(0);

    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = asked.clone();
    ui.global::<Player>().on_loop_cycled(move || {
        counter.set(counter.get() + 1);
    });

    loop_key(&ui, "循环: 关")
        .expect("找不到循环键")
        .invoke_accessible_default_action();

    assert_eq!(asked.get(), 1, "拨一下该喊一声");
    assert_eq!(
        ui.global::<Player>().get_loop_mode(),
        0,
        "值该纹丝不动 —— 写它是 Rust 的活,不是控件的"
    );
}

/// 循环键把三态念出来:关 / 列表 / 单曲,旧标签跟着退场。
#[test]
fn the_loop_button_labels_all_three_states() {
    let ui = wide_page();

    ui.global::<Player>().set_loop_mode(0);
    assert!(loop_key(&ui, "循环: 关").is_some());

    ui.global::<Player>().set_loop_mode(1);
    assert!(loop_key(&ui, "循环: 列表").is_some());
    assert!(
        loop_key(&ui, "循环: 关").is_none(),
        "换态之后旧标签不该还在"
    );

    ui.global::<Player>().set_loop_mode(2);
    assert!(loop_key(&ui, "循环: 单曲").is_some());
}

/// **日月开关退场**:控制簇里找不到它,主题唯一入口是设置页的三值选择。
/// 设置页(#61)落地后它本就该撤;第五颗循环键进场把紧凑行挤爆,正式送走。
#[test]
fn the_day_night_switch_is_gone_from_the_cluster() {
    let ui = wide_page();

    assert!(
        !present(&ui, "DayNightSwitch::touch"),
        "日月开关该已从控制簇退场,主题去设置页拨"
    );
}





/// 播放键换成环形进度键后,对外仍是那颗「播放/暂停」按钮:
/// 标签在、按一下喊 toggle-play、值由 Rust 写回。
#[test]
fn the_ring_play_key_still_toggles_playback() {
    let ui = wide_page();
    ui.global::<Player>().set_is_playing(false);

    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = asked.clone();
    ui.global::<Player>().on_toggle_play(move || {
        counter.set(counter.get() + 1);
    });

    testing::ElementHandle::find_by_accessible_label(
        &ui, "播放",
    )
    .next()
    .expect("找不到播放键")
    .invoke_accessible_default_action();

    assert_eq!(asked.get(), 1, "按一下该喊一声");
    assert!(
        !ui.global::<Player>().get_is_playing(),
        "值该纹丝不动 —— 写它是 Rust 的活"
    );
}

// 「拖动时不被每秒的位置刷新拽回去」这一条**没有测试**。
//
// 骨架里原本有它,写不出来:无头测试驱动不了真实指针,而 ProgressBar 既不是
// 独立编译的类型(build.rs 只编 app.slint)、又长在一个 `if` 里,Rust 侧
// 引用不到它的属性。造一条只断言"属性没被改写"的测试等于自欺,故不留。
//
// 那一行因此写成声明式的 `dragging ? drag-ratio : ratio`(见 controls.slint),
// 靠形状而不是靠断言;改它要手动验一次:拖住不放,看滑块会不会自己往回跳。
