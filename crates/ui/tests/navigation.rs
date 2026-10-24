//! 一级导航的界面行为(MEO-1876)。无头跑,与 controls.rs 同一套路。
//!
//! 九个目的地平级:没有 Home,也没有音乐页里那第二套导航。这里钉的是
//! 「摆了哪几项、什么顺序、点了去哪」—— 顺序与命名是用户在 MEO-1884 拍的板。
//! 标签一律断言全名:读屏念的、MCP 脚本按名字找的都是它,底栏上显示简称也一样。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use ui::MainWindow;
use ui::Player;
use ui::Session;
use ui::Shell;
use ui::Viz;

const WIDE_ORDER: [&str; 9] = [
    "每日推荐",
    "电台",
    "搜索",
    "我喜欢",
    "我的歌单",
    "最近播放",
    "已下载",
    "个人",
    "设置",
];
const BAR_ORDER: [&str; 5] =
    ["每日推荐", "搜索", "我喜欢", "我的歌单", "更多"];
const MORE_ORDER: [&str; 5] =
    ["电台", "最近播放", "已下载", "个人", "设置"];

fn present(ui: &MainWindow, id: &str) -> bool {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .is_some()
}

fn labels(ui: &MainWindow, id: &str) -> Vec<String> {
    testing::ElementHandle::find_by_element_id(ui, id)
        .filter_map(|e| {
            e.accessible_label().map(|s| s.to_string())
        })
        .collect()
}

fn find(
    ui: &MainWindow,
    id: &str,
    label: &str,
) -> testing::ElementHandle {
    testing::ElementHandle::find_by_element_id(ui, id)
        .find(|e| {
            e.accessible_label().is_some_and(|s| s == label)
        })
        .unwrap_or_else(|| {
            panic!("{id} 里找不到「{label}」")
        })
}

/// 登录之后的主窗口,摆一首歌:曲目列表只在有歌时出现(#116)。
fn window(compact: bool) -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_compact(compact);
    ui.global::<Player>().set_tracks(slint::ModelRc::new(
        slint::VecModel::from(
            vec![ui::TrackRow::default()],
        ),
    ));
    ui
}

/// 启动直接进每日推荐,树里没有启动器的瓦片,也没有壳的页签。
#[test]
fn launch_lands_on_daily_without_a_home() {
    let ui = window(false);

    assert_eq!(ui.global::<Shell>().get_destination(), 0);
    assert!(present(&ui, "MusicPage::track-list"));
    assert!(!present(&ui, "MainWindow::music-tile"));
    for shell_tab in ["Home", "Music"] {
        assert!(
            testing::ElementHandle::find_by_accessible_label(
                &ui, shell_tab
            )
            .next()
            .is_none(),
            "树里还有壳的页签「{shell_tab}」"
        );
    }
}

/// 宽版式侧栏九项,顺序与分组照 MEO-1884;第二套导航不在。
#[test]
fn the_sidebar_lists_nine_destinations_in_order() {
    let ui = window(false);

    assert_eq!(
        labels(&ui, "SideNavRow::touch"),
        WIDE_ORDER
    );
    assert!(!present(&ui, "MusicPage::music-rail"));
    assert!(!present(&ui, "MusicPage::music-bar"));
    assert!(
        !present(&ui, "NavItem::touch"),
        "宽版式没有底栏"
    );
}

/// 侧栏每一项都直达自己那一页,并报得出自己被选中了。
#[test]
fn every_sidebar_row_goes_straight_to_its_page() {
    let ui = window(false);

    for (label, id) in [
        ("电台", 4),
        ("搜索", 2),
        ("我喜欢", 6),
        ("我的歌单", 1),
        ("最近播放", 3),
        ("已下载", 5),
        ("个人", 7),
        ("设置", 8),
        ("每日推荐", 0),
    ] {
        find(&ui, "SideNavRow::touch", label)
            .invoke_accessible_default_action();
        assert_eq!(
            ui.global::<Shell>().get_destination(),
            id,
            "点「{label}」该去 {id}"
        );
        let row = find(&ui, "SideNavRow::touch", label);
        assert_eq!(
            row.accessible_checked(),
            Some(true),
            "{label}"
        );
    }
}

/// 「我喜欢」是一批歌,不是歌单列表;设置不摆音乐页。
#[test]
fn liked_and_settings_show_their_own_pages() {
    let ui = window(false);

    find(&ui, "SideNavRow::touch", "我喜欢")
        .invoke_accessible_default_action();
    assert!(present(&ui, "MusicPage::track-list"));
    assert!(!present(&ui, "MusicPage::playlist-list"));

    find(&ui, "SideNavRow::touch", "设置")
        .invoke_accessible_default_action();
    assert!(!present(&ui, "MusicPage::track-list"));
    assert!(present(&ui, "SettingsPage::logout-touch"));
}

/// 紧凑版式底栏五格,标签是全名;侧栏不在。
#[test]
fn the_bottom_bar_has_four_destinations_and_more() {
    let ui = window(true);

    assert_eq!(labels(&ui, "NavItem::touch"), BAR_ORDER);
    assert!(!present(&ui, "SideNavRow::touch"));
    assert!(
        !present(&ui, "MorePanel::item"),
        "面板默认收着"
    );
}

/// 「更多」平铺其余五项,点一项直达并收起面板,「更多」保持选中。
#[test]
fn more_opens_a_flat_panel_that_jumps_and_closes() {
    let ui = window(true);

    find(&ui, "NavItem::touch", "更多")
        .invoke_accessible_default_action();
    assert_eq!(labels(&ui, "MorePanel::item"), MORE_ORDER);

    find(&ui, "MorePanel::item", "设置")
        .invoke_accessible_default_action();
    assert_eq!(ui.global::<Shell>().get_destination(), 8);
    assert!(!present(&ui, "MorePanel::item"), "点完该收起");
    assert!(present(&ui, "SettingsPage::logout-touch"));
    assert_eq!(
        find(&ui, "NavItem::touch", "更多")
            .accessible_checked(),
        Some(true),
        "当前页在面板里时「更多」亮着"
    );
    assert_eq!(
        find(&ui, "NavItem::touch", "每日推荐")
            .accessible_checked(),
        Some(false)
    );
}

/// 底栏四格直达,「我喜欢」报自己的编号。
#[test]
fn bottom_bar_destinations_report_their_ids() {
    let ui = window(true);

    for (label, id) in [
        ("搜索", 2),
        ("我喜欢", 6),
        ("我的歌单", 1),
        ("每日推荐", 0),
    ] {
        find(&ui, "NavItem::touch", label)
            .invoke_accessible_default_action();
        assert_eq!(
            ui.global::<Shell>().get_destination(),
            id,
            "{label}"
        );
    }
}

/// 换目的地不动正在播的那首。
#[test]
fn switching_destinations_does_not_disturb_playback() {
    let ui = window(false);
    ui.global::<Viz>().set_now_title("紅蓮華".into());
    ui.global::<Player>().set_is_playing(true);

    find(&ui, "SideNavRow::touch", "我的歌单")
        .invoke_accessible_default_action();
    find(&ui, "SideNavRow::touch", "设置")
        .invoke_accessible_default_action();

    assert_eq!(
        ui.global::<Viz>().get_now_title(),
        "紅蓮華"
    );
    assert!(ui.global::<Player>().get_is_playing());
}
