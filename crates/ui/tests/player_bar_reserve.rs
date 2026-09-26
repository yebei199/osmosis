//! 守卫(#143):控制条浮在四个页签上面,每一页都得**自己记得**在底部
//! 留出 `BarMetrics.page-reserve`(条身 62 + 上下间隙各 22 = 106px)。
//! 个人页、音乐页、设置页各自踩过一次这个坑,这条测试把三页一起钉住 ——
//! 新加一个页签忘了留空,这里要挂,不必再等真机踩一次。
//!
//! 卡墙分区(tab 0)豁免:那一页是网格卡墙,本来就不走 Flickable /
//! 可滚列表,没有「最后一行滚不出条底」这回事。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use ui::Library;
use ui::MainWindow;
use ui::Player;
use ui::Session;
use ui::Shell;

/// `BarMetrics.page-reserve`:条身 62 + 上下两段 22。
const BAR_RESERVE: f32 = 62.0 + 22.0 * 2.0;

fn present(ui: &MainWindow, id: &str) -> bool {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .is_some()
}

fn window(tab: i32) -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_compact(false);
    ui.global::<Shell>().set_current_tab(tab);
    ui
}

/// Flickable 型页面(个人页、设置页):留空进 `column` 的 padding,
/// `column` 的 preferred-height 会跟着长 —— 量它在有条、没条两种状态
/// 下的高度差。
fn column_reserve(ui: &MainWindow, column_id: &str) -> f32 {
    let height = |ui: &MainWindow| {
        // 查一次把条件元素逼出来 —— 无头下它们惰性实例化。
        let _ = present(ui, column_id);
        testing::ElementHandle::find_by_element_id(
            ui, column_id,
        )
        .next()
        .unwrap_or_else(|| panic!("{column_id} 该在"))
        .size()
        .height
    };
    ui.global::<Player>().set_has_track(false);
    let without = height(ui);
    ui.global::<Player>().set_has_track(true);
    let with = height(ui);
    with - without
}

#[test]
fn profile_page_reserves_room_for_the_bar() {
    let ui = window(2);
    ui.global::<ui::Profile>().set_loaded(true);

    assert!(
        column_reserve(&ui, "ProfilePage::column")
            >= BAR_RESERVE,
        "个人页忘了照 BarMetrics.page-reserve 留空"
    );
}

#[test]
fn settings_page_reserves_room_for_the_bar() {
    let ui = window(3);

    assert!(
        column_reserve(&ui, "SettingsPage::column")
            >= BAR_RESERVE,
        "设置页忘了照 BarMetrics.page-reserve 留空"
    );
}

/// 音乐页不是 Flickable,留空进根的 padding-bottom —— 量法照 #116:
/// 歌单列表底边到页面底边的距离,有条要比没条宽出 `page-reserve` 那么多。
#[test]
fn music_page_reserves_room_for_the_bar() {
    let ui = window(1);
    ui.global::<Shell>().set_music_section(1);
    ui.global::<Library>()
        .set_open_playlist_name("".into());

    let blank_below = |ui: &MainWindow| -> f32 {
        let list = testing::ElementHandle::find_by_element_id(
            ui,
            "MusicPage::playlist-list",
        )
        .next()
        .expect("歌单列表该在");
        let page =
            testing::ElementHandle::find_by_element_type_name(
                ui, "MusicPage",
            )
            .next()
            .expect("音乐页该在");
        let bottom = |h: &testing::ElementHandle| {
            h.absolute_position().y + h.size().height
        };
        bottom(&page) - bottom(&list)
    };

    ui.global::<Player>().set_has_track(false);
    let without = blank_below(&ui);
    ui.global::<Player>().set_has_track(true);
    let with = blank_below(&ui);

    assert!(
        with - without >= BAR_RESERVE,
        "音乐页忘了照 BarMetrics.page-reserve 留空"
    );
}
