//! 卡墙视图切换的界面行为(#66,adr/0025)。无头跑,同一套路。
//!
//! 几何与动力学在 `ui::wall` 单测里钉;这里钉的是 .slint 那半:
//! 开关喊没喊、喊的是哪个视图,以及「无 GPU 构建整套控件都不存在」——
//! 那是 web / iOS 的默认样子,退化必须是静默的。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use slint::{ModelRc, VecModel};
use ui::Session;
use ui::{MainWindow, TrackRow};

fn row(id: &str) -> TrackRow {
    TrackRow {
        id: id.into(),
        title: "曲".into(),
        artists: "人".into(),
        duration: "03:00".into(),
        loading: false,
        liked: false,
        cover_url: format!("https://cdn/{id}.jpg").into(),
        cover: slint::Image::default(),
    }
}

fn music_page(supported: bool) -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.set_current_tab(1);
    ui.set_wall_supported(supported);
    ui.set_tracks(ModelRc::new(VecModel::from(vec![
        row("a"),
        row("b"),
    ])));
    ui
}

fn invoke(ui: &MainWindow, id: &str) {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .unwrap_or_else(|| panic!("找不到 {id}"))
        .invoke_accessible_default_action();
}

/// 两个视图键各喊各的档,值不自己动:显示态由 Rust 写回
/// (塌回动画落地才换视图,wall_drive.rs)。
#[test]
fn the_view_toggle_asks_without_flipping() {
    let ui = music_page(true);

    let asked =
        std::rc::Rc::new(std::cell::Cell::new(-1i32));
    let seen = asked.clone();
    ui.on_set_view_wall(move |to_wall| {
        seen.set(i32::from(to_wall));
    });

    invoke(&ui, "MainWindow::view-list-btn");
    assert_eq!(asked.get(), 0, "列表键该报 false");
    assert!(
        ui.get_view_wall(),
        "意图值该由 Rust 写,控件不自己置位"
    );

    invoke(&ui, "MainWindow::view-wall-btn");
    assert_eq!(asked.get(), 1, "卡墙键该报 true");
}

/// 无 GPU 构建(wall-supported 假)整套卡墙控件不存在:
/// 没有开关、没有场区,列表照常 —— 静默降级,不是报错。
#[test]
fn unsupported_builds_fall_back_to_the_list() {
    let ui = music_page(false);

    assert_eq!(
        testing::ElementHandle::find_by_element_id(
            &ui,
            "MainWindow::view-wall-btn",
        )
        .count(),
        0,
        "不支持时不该有卡墙开关"
    );
    assert_eq!(
        testing::ElementHandle::find_by_element_id(
            &ui,
            "MainWindow::wall-area",
        )
        .count(),
        0,
        "不支持时不该有卡墙场区"
    );
    assert!(
        testing::ElementHandle::find_by_element_id(
            &ui,
            "TrackList::cover-slot",
        )
        .count()
            > 0,
        "列表该照常在"
    );
}

/// 支持卡墙时,默认视图就是墙:场区在,列表让位(同一批卡的两种摆法,
/// 不同时出现)。
#[test]
fn the_wall_is_the_default_view_when_supported() {
    let ui = music_page(true);

    assert_eq!(
        testing::ElementHandle::find_by_element_id(
            &ui,
            "MainWindow::wall-area",
        )
        .count(),
        1,
        "默认该是卡墙"
    );
    assert_eq!(
        testing::ElementHandle::find_by_element_id(
            &ui,
            "TrackList::cover-slot",
        )
        .count(),
        0,
        "墙可见时列表该让位"
    );
}

/// **静止的墙每帧照渲。**
///
/// 旧省电门在动画收敛后让 frame() 给 None(冻结);前台恒满帧之后
/// (change_log 2026-08-11 always-on-rendering),只要场区量出了尺寸,
/// frame() 每次都给控制量 —— 连续调用也一样。
#[test]
fn a_settled_wall_still_renders_every_frame() {
    let ui = music_page(true);
    ui.set_wall_field_w(904.0);
    ui.set_wall_field_h(432.0);

    let mut drive = ui::WallDrive::new();
    // 走过收敛期,再连续多帧:每一帧都必须有控制量。
    for i in 0..300 {
        assert!(
            drive.frame(&ui).is_some(),
            "第 {i} 帧不该冻结"
        );
    }
}
