//! 导航玻璃药丸的几何。无头跑,与 controls.rs 同一套路。
//!
//! 视觉在 render3d 那侧(或 Slint 的 GlassPill 兜底),这里钉的是喂给它的那组数:
//! 条有多长、格心在哪、药丸走动时两条边有没有拉开。算错一格不会报错,
//! 只会让药丸停在隔壁那一项上。

use std::time::Duration;

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use ui::MainWindow;
use ui::Session;
use ui::Shell;

/// 登录之后才有导航。默认宽版式(侧栏)。
fn nav_window() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_compact(false);
    ui
}

/// 底栏第 i 格的横向范围。五格都是同一个 `NavItem`,按元素 id 取、按序号分。
fn item_span(ui: &MainWindow, i: usize) -> (f32, f32) {
    let item = testing::ElementHandle::find_by_element_id(
        ui,
        "NavItem::touch",
    )
    .nth(i)
    .unwrap_or_else(|| panic!("找不到第 {i} 个导航项"));
    let x = item.absolute_position().x;
    (x, x + item.size().width)
}

/// 侧栏里叫这个名字的那一行的纵向范围,相对侧栏顶(条首)。
fn row_span(ui: &MainWindow, label: &str) -> (f32, f32) {
    let row = testing::ElementHandle::find_by_element_id(
        ui,
        "SideNavRow::touch",
    )
    .find(|e| {
        e.accessible_label().is_some_and(|s| s == label)
    })
    .unwrap_or_else(|| panic!("侧栏里找不到「{label}」"));
    let top = testing::ElementHandle::find_by_element_id(
        ui,
        "SideNavRow::touch",
    )
    .next()
    .expect("侧栏一行都没有");
    // 第一行离条首是上内边距 22px 加品牌那 50px:用它反推条首,不依赖状态条有多高。
    let origin = top.absolute_position().y - 72.0;
    let y = row.absolute_position().y - origin;
    (y, y + row.size().height)
}

/// 读一遍药丸两条边。Slint 的 animate 要属性**先被求值过**才有旧值可动:
/// 真机上渲染循环每帧都读,测试里得自己补这一下,否则切换只会瞬移。
fn observe(ui: &MainWindow) {
    let _ = (ui.get_nav_lead(), ui.get_nav_lag());
}

fn go(ui: &MainWindow, destination: i32) {
    observe(ui);
    ui.global::<Shell>().set_destination(destination);
    // 慢的那条边是 620ms,给足时间让两条都到位。
    testing::mock_elapsed_time(Duration::from_millis(900));
}

/// 紧凑版式的底栏把自己的横条几何回写给 seam:宽由布局量出,
/// 高是 64px 加手势条 inset。宽版式仍走侧栏那一份竖条几何。
#[test]
fn the_bottom_bar_reports_a_horizontal_strip() {
    let ui = nav_window();
    assert!(!ui.get_nav_horizontal(), "宽版式该是竖条");
    assert_eq!(
        ui.get_nav_strip_w(),
        224.0,
        "宽版式的条宽就是侧栏宽"
    );

    ui.global::<Shell>().set_compact(true);
    // 底栏是条件页面,先查一次元素逼出实例化,init 才会跑。
    let _ = item_span(&ui, 0);

    assert!(ui.get_nav_horizontal(), "紧凑版式该是横条");
    assert!(
        ui.get_nav_strip_w() > 100.0,
        "底栏该把窗口宽度回写,实得 {}",
        ui.get_nav_strip_w()
    );
    assert_eq!(
        ui.get_nav_strip_h(),
        64.0,
        "底栏高是 64px 加 inset,测试里 inset 为 0"
    );
}

/// 药丸落在当前选中格上;「更多」里的目的地落在第五格。
#[test]
fn the_pill_lands_on_the_selected_bottom_item() {
    let ui = nav_window();
    ui.global::<Shell>().set_compact(true);
    let _ = item_span(&ui, 0);

    for (slot, destination) in
        [(0, 0), (1, 2), (2, 6), (3, 1), (4, 4), (4, 8)]
    {
        go(&ui, destination);
        let (left, right) = item_span(&ui, slot);
        let lead = ui.get_nav_lead();
        assert!(
            lead > left && lead < right,
            "目的地 {destination} 的药丸该落在第 {slot} 格 {left}..{right},实得 {lead}"
        );
    }
}

/// 侧栏轨道上十行(三组,组间隔一段,沉底那组贴着栏底),药丸落在选中那一行上。
#[test]
fn the_pill_lands_on_the_selected_sidebar_row() {
    let ui = nav_window();

    for (label, destination) in [
        ("电台", 4),
        ("搜索", 2),
        ("我喜欢", 6),
        ("我的歌单", 1),
        ("最近播放", 3),
        ("已下载", 5),
        ("个人", 7),
        ("已屏蔽", 9),
        ("设置", 8),
        ("每日推荐", 0),
    ] {
        go(&ui, destination);
        let (top, bottom) = row_span(&ui, label);
        let lead = ui.get_nav_lead();
        assert!(
            lead > top && lead < bottom,
            "「{label}」的药丸该落在 {top}..{bottom},实得 {lead}"
        );
        assert_eq!(
            lead,
            ui.get_nav_lag(),
            "到位之后两条边该重合"
        );
    }
}

/// 底栏五格全在轨道上:紧凑版式里停在设置,药丸照走到「更多」那格。
#[test]
fn the_bottom_bar_keeps_all_five_slots_on_the_track() {
    let ui = nav_window();
    ui.global::<Shell>().set_compact(true);
    let _ = item_span(&ui, 0);

    go(&ui, 8);

    let (left, right) = item_span(&ui, 4);
    let lead = ui.get_nav_lead();
    assert!(
        lead > left && lead < right,
        "底栏的药丸该落在第五格 {left}..{right},实得 {lead}"
    );
}

/// 沉底那两行报得出自己开着还是关着 —— 它们是 tab,读屏软件得念出「已选中」;
/// 点一下切到那一页。
#[test]
fn the_bottom_rows_are_checkable_tabs() {
    let ui = nav_window();

    let row = || {
        testing::ElementHandle::find_by_element_id(
            &ui,
            "SideNavRow::touch",
        )
        .find(|e| {
            e.accessible_label()
                .is_some_and(|s| s == "个人")
        })
        .expect("侧栏底部找不到「个人」")
    };
    assert_eq!(row().accessible_checkable(), Some(true));
    assert_eq!(row().accessible_checked(), Some(false));

    row().invoke_accessible_default_action();
    assert_eq!(
        ui.global::<Shell>().get_destination(),
        7,
        "点一下该切到个人"
    );
    assert_eq!(row().accessible_checked(), Some(true));
}

/// 走动的半途药丸被拉长:快的那条边已经冲在前面,慢的还落在后面。
/// 时长差没了就退化成一整块平移,没有那一下拉伸。
#[test]
fn the_pill_stretches_mid_transition() {
    let ui = nav_window();
    ui.global::<Shell>().set_compact(true);
    let _ = item_span(&ui, 0);

    // 从最左边一格往最右边走,只走一小段:快边 420ms、慢边 620ms。
    observe(&ui);
    ui.global::<Shell>().set_destination(8);
    testing::mock_elapsed_time(Duration::from_millis(120));

    let lead = ui.get_nav_lead();
    let lag = ui.get_nav_lag();
    assert!(
        lead > lag,
        "往右走的半途该是快边在前,实得 {lead} / {lag}"
    );
}
