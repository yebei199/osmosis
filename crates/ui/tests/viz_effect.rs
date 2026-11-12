//! 播放页的效果切换与场景图的铺法(docs/adr/0041)。无头跑。
//!
//! 效果本身画在 render3d 里(有它自己的单测),这里钉的是界面这一半:菜单开得出、
//! 收得起,点一行就把编号写进 `Viz.viz-effect` 并通知 Rust 落盘;场景图在两种版式下
//! 都铺满整窗 —— 曾经宽版式把它整张左移两成,右边那条没有任何 3D 内容。

use std::cell::Cell;
use std::rc::Rc;

use i_slint_backend_testing as testing;
use slint::ComponentHandle;
use ui::MainWindow;
use ui::Player;
use ui::Session;
use ui::Shell;
use ui::Viz;

/// 播放页展开的窗口。登录页盖住整个窗口、会吃掉指针,所以先登录。
fn play_window(
    width: f32,
    height: f32,
    compact: bool,
) -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.window()
        .set_size(slint::LogicalSize::new(width, height));
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_compact(compact);
    ui.global::<Shell>().set_play_page_open(true);
    ui.global::<Player>().set_has_track(true);
    ui
}

fn element(
    ui: &MainWindow,
    id: &str,
) -> Option<testing::ElementHandle> {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
}

fn options(ui: &MainWindow) -> Vec<testing::ElementHandle> {
    testing::ElementHandle::find_by_element_id(
        ui,
        "PlayPage::effect-option",
    )
    .collect()
}

fn open_menu(ui: &MainWindow) {
    element(ui, "PlayPage::effect-entry")
        .expect("播放页该有效果入口")
        .mock_single_click(
            slint::platform::PointerEventButton::Left,
        );
}

/// 默认是封面实物,菜单收着;入口写着当前效果的名字。
#[test]
fn the_sleeve_is_the_default_and_the_menu_starts_closed() {
    let ui = play_window(400.0, 800.0, true);
    assert_eq!(ui.global::<Viz>().get_viz_effect(), 0);
    assert!(
        options(&ui).is_empty(),
        "菜单不该一开始就开着"
    );
    let entry =
        testing::ElementHandle::find_by_accessible_label(
            &ui,
            "视觉效果:封面实物",
        )
        .next();
    assert!(entry.is_some(), "入口该报出当前效果的名字");
}

/// 点入口开菜单,一档一行;点第二行切到点云、收起菜单、通知 Rust 落盘。
#[test]
fn picking_a_row_switches_the_effect_and_closes_the_menu() {
    let ui = play_window(400.0, 800.0, true);
    let chosen = Rc::new(Cell::new(-1));
    {
        let chosen = chosen.clone();
        ui.global::<Viz>()
            .on_viz_effect_chosen(move |i| chosen.set(i));
    }

    open_menu(&ui);
    let rows = options(&ui);
    assert_eq!(rows.len(), 2, "两个效果各一行");

    rows[1].mock_single_click(
        slint::platform::PointerEventButton::Left,
    );
    assert_eq!(ui.global::<Viz>().get_viz_effect(), 1);
    assert_eq!(chosen.get(), 1, "选了要通知落盘");
    assert!(!ui.global::<Viz>().get_effect_menu_open());
    assert!(options(&ui).is_empty(), "选完菜单该收起");
}

/// 歌词页打开时入口让位:它与歌词页画在同一片区域。
#[test]
fn the_entry_hides_while_lyrics_are_open() {
    let ui = play_window(400.0, 800.0, true);
    assert!(
        element(&ui, "PlayPage::effect-entry").is_some()
    );
    ui.global::<Viz>().set_lyrics_page_open(true);
    assert!(
        element(&ui, "PlayPage::effect-entry").is_none()
    );
}

/// 场景图在两种版式下都铺满整窗:主体的偏左 / 偏上由 render3d 的相机移轴做,
/// 不再把整张图挪出窗外。
#[test]
fn the_scene_image_covers_the_whole_window() {
    for (width, height, compact) in
        [(1280.0, 800.0, false), (400.0, 800.0, true)]
    {
        let ui = play_window(width, height, compact);
        ui.global::<Viz>().set_viz_scene(slint::Image::from_rgba8(
            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(
                64, 64,
            ),
        ));
        let image = element(&ui, "PlayPage::scene-image")
            .expect("有场景图时该摆出来");
        let at = image.absolute_position();
        let size = image.size();
        assert_eq!(
            (at.x, at.y),
            (0.0, 0.0),
            "compact = {compact}"
        );
        assert_eq!(
            (size.width, size.height),
            (width, height),
            "compact = {compact}"
        );
    }
}
