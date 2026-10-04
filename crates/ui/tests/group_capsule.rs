//! 播放组从完整提示收成胶囊，指针面板和壳页控件共享真实布局。

use std::{cell::Cell, rc::Rc, time::Duration};

use i_slint_backend_testing as testing;
use similar_asserts::assert_eq;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, ModelRc, VecModel};
use ui::{
    DeviceRow, Library, MainWindow, Player, PlaylistRow,
    Session, Shell, Theme, TrackRow,
};

/// 填入真实页面的数据，使几何断言能碰到列表行和设备信息。
fn window(width: f32, height: f32) -> MainWindow {
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.window()
        .set_size(slint::LogicalSize::new(width, height));
    ui.global::<Shell>().set_compact(width < 600.0);
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_music_section(1);
    ui.global::<Player>().set_tracks(ModelRc::new(
        VecModel::from(vec![TrackRow {
            id: "track".into(),
            title: "测试曲".into(),
            ..Default::default()
        }]),
    ));
    ui.global::<Library>().set_playlists(ModelRc::new(
        VecModel::from(vec![PlaylistRow {
            id: "playlist".into(),
            name: "测试歌单".into(),
            ..Default::default()
        }]),
    ));
    ui.global::<Shell>().set_devices(ModelRc::new(
        VecModel::from(vec![
            DeviceRow {
                id: "pc1".into(),
                name: "pc1".into(),
                member: true,
                ..Default::default()
            },
            DeviceRow {
                id: "pc2".into(),
                name: "pc2".into(),
                member: false,
                ..Default::default()
            },
        ]),
    ));
    ui
}

/// 按组件id找真实元素，缺失直接失败。
fn element(
    ui: &MainWindow,
    id: &str,
) -> testing::ElementHandle {
    advance(0);
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .unwrap_or_else(|| panic!("找不到 {id}"))
}

/// 可见树决定当前形态，隐藏的旧元素不能冒充胶囊。
fn present(ui: &MainWindow, id: &str) -> bool {
    advance(0);
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .is_some()
}

/// 推模拟时钟，Timer和过渡动画仍由真实Slint执行。
fn advance(milliseconds: u64) {
    testing::mock_elapsed_time(Duration::from_millis(
        milliseconds,
    ));
}

/// 从组投影出现进入，先量布局使计时绑定得到求值。
fn enter(ui: &MainWindow) {
    ui.global::<Shell>()
        .set_output_text("输出: pc1".into());
    ui.global::<Shell>()
        .set_group_banner("正在遥控 pc1".into());
    assert!(
        element(ui, "MainWindow::group-strip")
            .size()
            .height
            >= 44.0
    );
}

/// 走完四秒提示和二百毫秒收起过渡。
fn collapse(ui: &MainWindow) {
    enter(ui);
    advance(4000);
    advance(200);
    assert!(present(ui, "MainWindow::group-capsule"));
    assert!(!present(ui, "MainWindow::group-strip"));
}

/// 原生指针事件走命中测试，不能绕过覆层。
fn click(
    ui: &MainWindow,
    position: slint::LogicalPosition,
) {
    ui.window().dispatch_event(WindowEvent::PointerMoved {
        position,
    });
    ui.window().dispatch_event(
        WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        },
    );
    ui.window().dispatch_event(
        WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        },
    );
}

/// 点击元素中心，坐标从本轮真实布局读取。
fn click_element(
    ui: &MainWindow,
    handle: &testing::ElementHandle,
) {
    let p = handle.absolute_position();
    let size = handle.size();
    click(
        ui,
        slint::LogicalPosition::new(
            p.x + size.width / 2.0,
            p.y + size.height / 2.0,
        ),
    );
}

/// 零宽或零高的元素不能遮挡胶囊。
fn positive_area(size: slint::LogicalSize) -> bool {
    size.width > 0.0 && size.height > 0.0
}

/// 两段边界相接不算相交；同一规则用于横纵两轴。
fn axis_overlap(
    a_start: f32,
    a_span: f32,
    b_start: f32,
    b_span: f32,
) -> bool {
    a_start < b_start + b_span && b_start < a_start + a_span
}

/// 取矩形相交，边界相接允许。
fn overlaps(
    a: &testing::ElementHandle,
    b: &testing::ElementHandle,
) -> bool {
    let ap = a.absolute_position();
    let bp = b.absolute_position();
    let az = a.size();
    let bz = b.size();
    positive_area(az)
        && positive_area(bz)
        && axis_overlap(ap.x, az.width, bp.x, bz.width)
        && axis_overlap(ap.y, az.height, bp.y, bz.height)
}

/// 胶囊与指针控件、输入框、具无障碍操作的组件都不相交。
fn assert_clear(ui: &MainWindow) {
    let capsule = element(ui, "MainWindow::group-capsule");
    assert!(
        capsule.size().width >= 44.0
            && capsule.size().height >= 44.0
    );
    for kind in [
        "TouchArea",
        "LineEdit",
        "NavItem",
        "RoundControl",
        "HoverButton",
    ] {
        for other in testing::ElementHandle::find_by_element_type_name(ui, kind) {
            // 输入探针只记录指针，没有点击动作；胶囊自身不计为遮挡。
            if other.id().as_deref() == Some("MainWindow::input-probe") || other.accessible_label().as_deref() == Some("播放组") { continue; }
            assert!(!overlaps(&capsule, &other), "胶囊压到 {kind} {:?} at {:?}", other.accessible_label(), other.absolute_position());
        }
    }
}

/// 四秒之前保留完整提示，动画完成后页面回到无组起点。
#[test]
fn the_group_collapses_after_four_seconds_without_reserving_a_row()
 {
    testing::init_no_event_loop();
    for width in [420.0, 1000.0] {
        let ui = window(width, 800.0);
        let page = element(&ui, "MainWindow::page-stack");
        let baseline = page.absolute_position().y;
        enter(&ui);
        assert!(page.absolute_position().y > baseline);
        advance(3900);
        assert!(present(&ui, "MainWindow::group-strip"));
        advance(100);
        advance(200);
        assert!(present(&ui, "MainWindow::group-capsule"));
        assert!(!present(&ui, "MainWindow::group-strip"));
        assert_eq!(page.absolute_position().y, baseline);
        advance(5000);
        assert!(present(&ui, "MainWindow::group-capsule"));
    }
}

/// 文案、输出和组故障变化分别重新给完整四秒，普通同值刷新不重新计时。
#[test]
fn projected_group_changes_restart_the_full_notice() {
    testing::init_no_event_loop();
    let ui = window(420.0, 800.0);
    collapse(&ui);
    ui.global::<Shell>()
        .set_group_banner("与 pc1 一起播放".into());
    assert!(present(&ui, "MainWindow::group-strip"));
    advance(3000);
    ui.global::<Shell>()
        .set_output_text("输出: 本机、pc1".into());
    assert!(present(&ui, "MainWindow::group-strip"));
    advance(3900);
    assert!(present(&ui, "MainWindow::group-strip"));
    advance(100);
    advance(200);
    assert!(present(&ui, "MainWindow::group-capsule"));
    ui.global::<Shell>()
        .set_group_text("pc1 路由未校准".into());
    assert!(present(&ui, "MainWindow::group-strip"));
    advance(3000);
    ui.global::<Shell>()
        .set_group_text("pc1 路由未校准".into());
    advance(1000);
    advance(200);
    assert!(present(&ui, "MainWindow::group-capsule"));
}

/// 组状态清除后所有形态消失，旧Timer不能把胶囊重新带回来。
#[test]
fn clearing_the_group_closes_the_panel_and_cancels_the_notice()
 {
    testing::init_no_event_loop();
    let ui = window(420.0, 800.0);
    collapse(&ui);
    click_element(
        &ui,
        &element(&ui, "MainWindow::group-capsule"),
    );
    assert!(present(&ui, "MainWindow::group-panel"));
    ui.global::<Shell>().set_group_banner("".into());
    advance(5000);
    for id in [
        "MainWindow::group-strip",
        "MainWindow::group-capsule",
        "MainWindow::group-panel",
    ] {
        assert!(!present(&ui, id));
    }
    enter(&ui);
    advance(3900);
    assert!(present(&ui, "MainWindow::group-strip"));
}

/// 面板读真实名册与输出投影；内部点击保留面板，遮罩指针关闭。
#[test]
fn a_pointer_opens_the_device_panel_and_the_backdrop_closes_it()
 {
    testing::init_no_event_loop();
    for width in [420.0, 1000.0] {
        let ui = window(width, 800.0);
        collapse(&ui);
        click_element(
            &ui,
            &element(&ui, "MainWindow::group-capsule"),
        );
        let panel = element(&ui, "MainWindow::group-panel");
        for text in ["pc1", "pc2", "输出: pc1"] {
            assert!(testing::ElementHandle::find_by_accessible_label(&ui, text).next().is_some(), "面板缺少{text}");
        }
        let p = panel.absolute_position();
        click(
            &ui,
            slint::LogicalPosition::new(
                p.x + 10.0,
                p.y + 10.0,
            ),
        );
        assert!(present(&ui, "MainWindow::group-panel"));
        click(&ui, slint::LogicalPosition::new(4.0, 4.0));
        assert!(!present(&ui, "MainWindow::group-panel"));
        assert!(present(&ui, "MainWindow::group-capsule"));
    }
}

/// 加入和退出来自同一真实按钮，各触发一次既有回调且触控区不小于44px。
#[test]
fn the_panel_routes_join_and_leave_through_the_existing_actions()
 {
    testing::init_no_event_loop();
    for joinable in [false, true] {
        let ui = window(420.0, 800.0);
        let joined = Rc::new(Cell::new(0));
        let left = Rc::new(Cell::new(0));
        let sink = joined.clone();
        ui.global::<Shell>().on_join_group(move || {
            sink.set(sink.get() + 1)
        });
        let sink = left.clone();
        ui.global::<Shell>().on_leave_group(move || {
            sink.set(sink.get() + 1)
        });
        ui.global::<Shell>().set_group_joinable(joinable);
        collapse(&ui);
        click_element(
            &ui,
            &element(&ui, "MainWindow::group-capsule"),
        );
        let label = if joinable {
            "加入播放组"
        } else {
            "退出组"
        };
        let action = testing::ElementHandle::find_by_accessible_label(&ui, label).next().expect("面板缺少组操作");
        assert!(
            action.size().height >= 44.0
                && action.size().width >= 44.0
        );
        click_element(&ui, &action);
        assert_eq!(
            (joined.get(), left.get()),
            if joinable { (1, 0) } else { (0, 1) }
        );
    }
}

/// 胶囊不能盖住420宽下的任何壳页头部控件，包括搜索与六种音乐分区。
#[test]
fn the_compact_capsule_avoids_controls_on_every_shell_page()
{
    testing::init_no_event_loop();
    for tab in 0..4 {
        for section in 0..6 {
            let ui = window(420.0, 800.0);
            ui.global::<Shell>().set_current_tab(tab);
            ui.global::<Shell>().set_music_section(section);
            ui.global::<ui::Downloads>()
                .set_active(section == 5);
            collapse(&ui);
            assert_clear(&ui);
            let capsule =
                element(&ui, "MainWindow::group-capsule");
            assert_eq!(
                capsule.absolute_position().x,
                360.0
            );
            assert_eq!(capsule.absolute_position().y, 16.0);
        }
    }
}

/// 宽版600高时，胶囊与上下两组导航和各音乐分区操作都分开。
#[test]
fn the_wide_capsule_avoids_navigation_even_in_a_short_window()
 {
    testing::init_no_event_loop();
    for height in [600.0, 800.0] {
        for section in 0..6 {
            let ui = window(1000.0, height);
            ui.global::<Shell>().set_music_section(section);
            ui.global::<ui::Downloads>()
                .set_active(section == 5);
            collapse(&ui);
            assert_clear(&ui);
            let capsule =
                element(&ui, "MainWindow::group-capsule");
            assert_eq!(capsule.absolute_position().x, 26.0);
            assert_eq!(
                capsule.absolute_position().y,
                164.0
            );
        }
    }
}

/// 34px旧行升为44px，独奏时占满原宽，有组时仅头部横向让出56px。
#[test]
fn the_compact_header_reserves_a_slot_only_while_a_group_exists()
 {
    testing::init_no_event_loop();
    let ui = window(420.0, 800.0);
    let full_width =
        element(&ui, "MusicPage::music-bar").size().width;
    assert_eq!(
        element(&ui, "MusicPage::music-bar").size().height,
        44.0
    );
    collapse(&ui);
    assert_eq!(
        element(&ui, "MusicPage::music-bar").size().width,
        full_width - 56.0
    );
    ui.global::<Shell>().set_group_banner("".into());
    assert_eq!(
        element(&ui, "MusicPage::music-bar").size().width,
        full_width
    );
}

/// 原错误和下载条保持流布局；胶囊让位消失只回到这两条下面。
#[test]
fn collapsing_the_group_keeps_error_and_download_space() {
    testing::init_no_event_loop();
    for width in [420.0, 1000.0] {
        let ui = window(width, 800.0);
        ui.global::<Shell>()
            .set_banner_text("网络断开".into());
        ui.global::<Shell>()
            .set_download_text("下载中 42%".into());
        let baseline =
            element(&ui, "MainWindow::page-stack")
                .absolute_position()
                .y;
        let error = element(&ui, "MainWindow::banner");
        let download =
            element(&ui, "MainWindow::download-strip");
        let original_error =
            (error.absolute_position(), error.size());
        let original_download =
            (download.absolute_position(), download.size());
        collapse(&ui);
        assert_eq!(
            element(&ui, "MainWindow::page-stack")
                .absolute_position()
                .y,
            baseline
        );
        assert_eq!(
            (error.absolute_position(), error.size()),
            original_error
        );
        assert_eq!(
            (download.absolute_position(), download.size()),
            original_download
        );
        assert_clear(&ui);
    }
}

/// 深浅主题下胶囊仍有可见24px图标，点开路径一致。
#[test]
fn the_capsule_has_a_visible_icon_in_both_themes() {
    testing::init_no_event_loop();
    for dark in [false, true] {
        let ui = window(420.0, 800.0);
        ui.global::<Theme>().set_dark(dark);
        collapse(&ui);
        let icon = element(&ui, "MainWindow::group-icon");
        assert_eq!(
            (icon.size().width, icon.size().height),
            (24.0, 24.0)
        );
        assert!(icon.computed_opacity() > 0.0);
        click_element(
            &ui,
            &element(&ui, "MainWindow::group-capsule"),
        );
        assert!(present(&ui, "MainWindow::group-panel"));
    }
}

/// 用户读面板时不被计时收走；实时输出变化在同一面板里可读。
#[test]
fn an_open_panel_stays_open_while_group_details_change() {
    testing::init_no_event_loop();
    let ui = window(420.0, 800.0);
    collapse(&ui);
    click_element(
        &ui,
        &element(&ui, "MainWindow::group-capsule"),
    );
    ui.global::<Shell>()
        .set_output_text("输出: pc2".into());
    advance(5000);
    assert!(present(&ui, "MainWindow::group-panel"));
    assert!(
        testing::ElementHandle::find_by_accessible_label(
            &ui,
            "输出: pc2"
        )
        .next()
        .is_some()
    );
}

/// 完整提示正在按下时不销毁触控目标，松手后的四秒再收起。
#[test]
fn a_pressed_notice_action_survives_the_collapse_deadline()
{
    testing::init_no_event_loop();
    let ui = window(420.0, 800.0);
    enter(&ui);
    let action =
        testing::ElementHandle::find_by_accessible_label(
            &ui, "退出",
        )
        .next()
        .expect("展开提示保留退出键");
    let p = action.absolute_position();
    let z = action.size();
    let position = slint::LogicalPosition::new(
        p.x + z.width / 2.0,
        p.y + z.height / 2.0,
    );
    ui.window().dispatch_event(WindowEvent::PointerMoved {
        position,
    });
    ui.window().dispatch_event(
        WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        },
    );
    advance(0);
    advance(5000);
    assert!(present(&ui, "MainWindow::group-strip"));
    ui.window().dispatch_event(
        WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        },
    );
    advance(0);
    advance(3900);
    assert!(present(&ui, "MainWindow::group-strip"));
    advance(100);
    advance(200);
    assert!(present(&ui, "MainWindow::group-capsule"));
}

/// 故障标志单独变化时重展，同值刷新不会延长提示。
#[test]
fn a_connection_fault_reopens_the_notice_without_a_text_change()
 {
    testing::init_no_event_loop();
    let ui = window(420.0, 800.0);
    collapse(&ui);
    ui.global::<Shell>().set_output_stale(true);
    assert!(present(&ui, "MainWindow::group-strip"));
    advance(3000);
    ui.global::<Shell>().set_output_stale(true);
    advance(1000);
    advance(200);
    assert!(present(&ui, "MainWindow::group-capsule"));
    ui.global::<Shell>().set_output_stale(false);
    assert!(present(&ui, "MainWindow::group-strip"));
}
