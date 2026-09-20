//! 控制条:主条 + 抽屉(#84/#85/#86)。无头跑,与 controls.rs 同一套路。
//!
//! 这一组管的是「一根条到处一样、抽屉装得下、开关看得出」。
//! 进度算得对不对仍由 progress::ratio 那边证。

use i_slint_backend_testing as testing;
use slint::ComponentHandle;
use ui::MainWindow;
use ui::Player;
use ui::Session;
use ui::Shell;

/// 除播放页外的四个页签。播放页是覆层,由 `play-page-open` 单开。
const PAGE_TABS: [i32; 4] = [0, 1, 2, 3];

fn key(
    ui: &MainWindow,
    label: &str,
) -> Option<testing::ElementHandle> {
    testing::ElementHandle::find_by_accessible_label(
        ui, label,
    )
    .next()
}

fn ids(
    ui: &MainWindow,
    id: &str,
) -> Vec<testing::ElementHandle> {
    testing::ElementHandle::find_by_element_id(ui, id)
        .collect()
}

fn present(ui: &MainWindow, id: &str) -> bool {
    !ids(ui, id).is_empty()
}

/// 把窗口调到手机那么窄。**只拨 `compact` 那一位是不够的** ——
/// 它只说明「该用紧凑版式」,窗口宽度不跟着变,条照样是宽的,
/// 于是所有关于让位的断言都会假通过。
fn narrow(ui: &MainWindow) {
    ui.window()
        .set_size(slint::LogicalSize::new(360.0, 780.0));
    ui.global::<Shell>().set_compact(true);
}

/// 登录、有歌、宽版式。条要这三样齐了才摆得出来。
fn playing_app() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_compact(false);
    ui.global::<Player>().set_has_track(true);
    ui
}

/// 条身那颗胶囊此刻的几何。没有条时返回 None。
fn bar_box(
    ui: &MainWindow,
) -> Option<(f32, f32, f32, f32)> {
    let h =
        ids(ui, "PlayerBar::capsule").into_iter().next()?;
    let p = h.absolute_position();
    let s = h.size();
    Some((p.x, p.y, s.width, s.height))
}

/// 按无障碍标签取一颗键的直径(宽)。
fn key_width(ui: &MainWindow, label: &str) -> f32 {
    key(ui, label)
        .unwrap_or_else(|| panic!("找不到「{label}」"))
        .size()
        .width
}

/// 环形播放键的直径。它的标签随播放态在「播放 / 暂停」之间换,
/// 任一时刻只有一个在场 —— 探针得认这一点,不能两个都要。
fn ring_width(ui: &MainWindow) -> f32 {
    key(ui, "播放")
        .or_else(|| key(ui, "暂停"))
        .expect("找不到环形播放键")
        .size()
        .width
}

/// 往窗口上某一点真按一下,走命中测试。
///
/// **这是 `invoke_accessible_default_action` 做不到的事。** 那个是对着元素直接调,
/// 盖在它上面的东西一概不参与;而用户的手指得穿过那一层。凡是「点得到吗」的断言
/// 都要走这条路 —— 一层透明的 TouchArea 压在上面时,只有这条会红。
fn click_point(ui: &MainWindow, x: f32, y: f32) {
    use slint::platform::PointerEventButton;
    use slint::platform::WindowEvent;

    let position = slint::LogicalPosition::new(x, y);
    let button = PointerEventButton::Left;
    let win = ui.window();
    win.dispatch_event(WindowEvent::PointerMoved {
        position,
    });
    win.dispatch_event(WindowEvent::PointerPressed {
        position,
        button,
    });
    win.dispatch_event(WindowEvent::PointerReleased {
        position,
        button,
    });
}

/// 往某个元素的正中真按一下。
fn click_at(ui: &MainWindow, h: &testing::ElementHandle) {
    let p = h.absolute_position();
    let s = h.size();
    click_point(
        ui,
        p.x + s.width / 2.0,
        p.y + s.height / 2.0,
    );
}

/// 抽屉此刻的样子:高度,加上逐行的无障碍标签。
/// 抽屉开合是每根条各记各的,所以这里自己先开一次。
fn drawer_shape(ui: &MainWindow) -> (f32, Vec<String>) {
    key(ui, "更多")
        .expect("找不到抽屉键")
        .invoke_accessible_default_action();
    let h = ids(ui, "PlayerBar::drawer")
        .into_iter()
        .next()
        .expect("抽屉该开着")
        .size()
        .height;
    let rows = ids(ui, "DrawerRow::touch")
        .iter()
        .filter_map(|r| {
            r.accessible_label().map(|s| s.to_string())
        })
        .collect();
    (h, rows)
}

// 断言分两份:主条本身(#85)在 `bar.rs`,抽屉那一层(#86)在 `drawer.rs`。
// 上面这些探针两边都要,所以留在这里 —— 子模块看得见 crate 根的私有项。
mod bar;
mod drawer;
