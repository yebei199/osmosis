//! 导航水滴选中器的几何。无头跑,与 controls.rs 同一套路。
//!
//! 视觉在 render3d 那侧、没有 GPU 可测,这里钉的是喂给它的那组数:
//! 条有多长、格心在哪、三球有没有拉开。算错一格不会报错,只会让水滴
//! 停在隔壁那一项上。

use std::time::Duration;

use i_slint_backend_testing as testing;
use ui::MainWindow;

/// 登录之后才有导航。默认宽版式(侧栏)。
fn nav_window() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.set_logged_in(true);
    ui.set_compact(false);
    ui
}

/// 底栏第 i 项的横向范围。四项都是同一个 `NavItem`,标签是内部的 `Text`
/// (设置页的标题也叫「设置」,按标签找会先撞上那个),所以按元素 id 取、按序号分。
fn item_span(ui: &MainWindow, i: usize) -> (f32, f32) {
    let item =
        testing::ElementHandle::find_by_element_id(
            ui,
            "NavItem::touch",
        )
        .nth(i)
        .unwrap_or_else(|| panic!("找不到第 {i} 个导航项"));
    let x = item.absolute_position().x;
    (x, x + item.size().width)
}

/// 读一遍三球位置。Slint 的 animate 要属性**先被求值过**才有旧值可动:
/// 真机上渲染循环每帧都读,测试里得自己补这一下,否则切 tab 只会瞬移。
fn observe(ui: &MainWindow) {
    let _ = (
        ui.get_nav_lead(),
        ui.get_nav_lag(),
        ui.get_nav_drop(),
    );
}

/// 紧凑版式的底栏把自己的横条几何回写给 seam:宽由布局量出,
/// 高是 64px 加手势条 inset。宽版式仍走侧栏那一份竖条几何。
#[test]
fn the_bottom_bar_reports_a_horizontal_strip() {
    let ui = nav_window();
    assert!(!ui.get_nav_horizontal(), "宽版式该是竖条");
    assert_eq!(
        ui.get_nav_strip_w(),
        96.0,
        "宽版式的条宽就是侧栏宽"
    );

    ui.set_compact(true);
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

/// 水滴落在当前选中项上:切 tab、动画走完后,头球中心落在那一格的
/// 横向范围内。四项等分,格心算错一格不报错,只会让水滴停在隔壁。
#[test]
fn the_droplet_lands_on_the_selected_bottom_item() {
    let ui = nav_window();
    ui.set_compact(true);
    let _ = item_span(&ui, 0);

    for tab in 0..4 {
        observe(&ui);
        ui.set_current_tab(tab);
        // 最慢那颗是 680ms,给足时间让三球都到位。
        testing::mock_elapsed_time(Duration::from_millis(
            900,
        ));

        let (left, right) = item_span(&ui, tab as usize);
        let lead = ui.get_nav_lead();
        assert!(
            lead > left && lead < right,
            "tab {tab} 的头球该落在 {left}..{right} 之间,实得 {lead}"
        );
    }
}

/// 侧栏底部两颗退出水滴轨道(#71):选中个人/设置时水滴停在最后一格主项的
/// 位置上并缩没(球体缩放归零),不再滑到栏底;切回来又长出来。
#[test]
fn the_droplet_melts_away_for_the_bottom_round_keys() {
    let ui = nav_window();

    observe(&ui);
    ui.set_current_tab(1);
    testing::mock_elapsed_time(Duration::from_millis(900));
    assert!(
        ui.get_nav_ball() > 0.9,
        "停在 Music 时水滴该是满的,实得 {}",
        ui.get_nav_ball()
    );
    let on_music = ui.get_nav_lead();

    observe(&ui);
    ui.set_current_tab(3);
    testing::mock_elapsed_time(Duration::from_millis(900));
    assert_eq!(
        ui.get_nav_ball(),
        0.0,
        "选中设置时水滴该缩没"
    );
    assert_eq!(
        ui.get_nav_lead(),
        on_music,
        "水滴不该滑到栏底,该停在最后一格主项上"
    );

    observe(&ui);
    ui.set_current_tab(0);
    testing::mock_elapsed_time(Duration::from_millis(900));
    assert!(
        ui.get_nav_ball() > 0.9,
        "切回主项该重新长出来,实得 {}",
        ui.get_nav_ball()
    );
}

/// 底栏那四格不受影响:紧凑版式里个人/设置仍在轨道上,水滴照走。
#[test]
fn the_bottom_bar_keeps_all_four_slots_on_the_track() {
    let ui = nav_window();
    ui.set_compact(true);
    let _ = item_span(&ui, 0);

    observe(&ui);
    ui.set_current_tab(3);
    testing::mock_elapsed_time(Duration::from_millis(900));

    assert!(
        ui.get_nav_ball() > 0.9,
        "底栏四格都在轨道上,水滴不该缩没"
    );
    let (left, right) = item_span(&ui, 3);
    let lead = ui.get_nav_lead();
    assert!(
        lead > left && lead < right,
        "底栏的头球该落在第四格 {left}..{right},实得 {lead}"
    );
}

/// 圆钮报得出自己开着还是关着 —— 它是 tab,读屏软件得念出「已选中」,
/// 而不是只有一个亮圆;点一下切到那一页,换掉 NavItem 之后这条线不能断。
#[test]
fn the_bottom_round_keys_are_checkable_tabs() {
    let ui = nav_window();
    // 停在 Home:个人主页没建,「个人」这个标签全应用只有侧栏那颗。
    ui.set_current_tab(0);

    let key =
        testing::ElementHandle::find_by_accessible_label(
            &ui, "个人",
        )
        .next()
        .expect("侧栏底部找不到「个人」圆钮");
    assert_eq!(
        key.accessible_checkable(),
        Some(true),
        "它是 tab,得报得出有没有被选中"
    );
    assert_eq!(
        key.accessible_checked(),
        Some(false),
        "还没切过去,该是关着的"
    );

    key.invoke_accessible_default_action();
    assert_eq!(
        ui.get_current_tab(),
        2,
        "点一下该切到个人主页"
    );
}

/// 三球中途仍拉得开:切 tab 的半途读三个位置,头球最靠前、小水滴最靠后。
/// 颈就是这个先后差被 smin 连出来的,时长差没了就退化成一整块平移。
#[test]
fn the_three_balls_stay_strung_out_mid_transition() {
    let ui = nav_window();
    ui.set_compact(true);
    let _ = item_span(&ui, 0);

    // 从最左边一格往最右边走,只走一小段:头球 240ms、尾球 440ms、小水滴 680ms。
    observe(&ui);
    ui.set_current_tab(3);
    testing::mock_elapsed_time(Duration::from_millis(120));

    let lead = ui.get_nav_lead();
    let lag = ui.get_nav_lag();
    let drop = ui.get_nav_drop();
    assert!(
        lead > lag && lag > drop,
        "往右走的半途该是头球在前、小水滴在后,实得 {lead} / {lag} / {drop}"
    );
}
