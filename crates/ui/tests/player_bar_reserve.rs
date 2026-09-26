//! 守卫(#143):控制条浮在页签上面,每一页都得**自己记得**在底部
//! 留出 `BarMetrics.page-reserve`(条身 62 + 上下间隙各 22 = 106px)。
//! 个人页、音乐页、设置页各自踩过一次这个坑,前三条测试把这三页钉住。
//!
//! 这三条测试本身**认不出**将来新加的第四个页签 —— 新页不在这份清单里,
//! 没人会为它添一条测试,这三条也不会跑到它。真正兜住「新页忘了留空」的
//! 是最后那条 `every_tab_is_either_covered_or_exempted`:它从 `Nav`(导航
//! 真正在用的那份条目表)读出页签总数,与「已覆盖 + 已豁免」的显式清单
//! 长度比对,对不上就挂 —— 逼着新增页签的人把它加进清单里。
//!
//! 卡墙分区(tab 0)豁免:那一页是网格卡墙,本来就不走 Flickable /
//! 可滚列表,没有「最后一行滚不出条底」这回事。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use slint::Model as _;
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
        let list =
            testing::ElementHandle::find_by_element_id(
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

    // 音乐页没条时底部本就留 16px(常规页边距),不是 0 —— 差值因此是
    // `page-reserve - 16px`,不是整段 `BAR_RESERVE`(见 musicpage.slint
    // 的 padding-bottom)。这里只钉「条身那 62px 至少留出来了」,
    // 忘了整段留空(差值退回 0)照样会挂。
    assert!(
        with - without >= 62.0,
        "音乐页忘了照 BarMetrics.page-reserve 留空"
    );
}

/// 绊线:页签总数必须等于「已覆盖 + 已豁免」这份显式清单的长度。
///
/// `Nav.items` + `Nav.bottom-items` 是导航栏真正在用的那份条目表 ——
/// 新加一个页签,这里的行数会跟着涨。清单不会自己跟着涨,于是两边对不上,
/// 这条测试就挂:提醒把新页纳入上面某条测试,或者显式豁免并写明理由。
/// 不这样兜底的话,上面三条各自钉一页的测试对第四个页签视而不见 ——
/// 没人会为它添新测试,这三条也不会跑到它,守卫形同虚设(#143 R-1)。
#[test]
fn every_tab_is_either_covered_or_exempted() {
    let ui = window(0);
    let nav = ui.global::<ui::Nav>();
    let total = nav.get_items().row_count()
        + nav.get_bottom_items().row_count();

    // 覆盖:上面三条测试各自钉住的页签。
    let covered = ["MusicPage", "ProfilePage", "SettingsPage"];
    // 豁免:卡墙分区(tab 0),理由见文件头注释。
    let exempted = ["WallView(卡墙,tab 0)"];

    assert_eq!(
        total,
        covered.len() + exempted.len(),
        "页签总数是 {total},但「覆盖 + 豁免」清单只认领了 {} 个 —— \
         新加的页签还没被这份清单认领,把它加进 covered 或 exempted(写明理由)",
        covered.len() + exempted.len()
    );
}
