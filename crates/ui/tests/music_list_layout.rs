//! 音乐页列表层的几何(#130)。无头跑,量的是布局给出的真实位置与尺寸。
//!
//! 这几条毛病肉眼看是「压住了」「多一条缝」「标题全是…」,落到数上都是
//! 某个元素的边没对上另一个元素的边 —— 所以断言的是边,不是截图。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use ui::{MainWindow, Session, Shell};

/// 宽版式音乐页,窗口给定逻辑尺寸。版式直接给定:无头后端里宽度推不出它
/// (见 music_nav.rs)。
fn music_page(width: f32, height: f32, section: i32) -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.window()
        .set_size(slint::LogicalSize::new(width, height));
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_compact(false);
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_music_section(section);
    ui
}

fn element(ui: &MainWindow, id: &str) -> testing::ElementHandle {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .unwrap_or_else(|| panic!("找不到 {id}"))
}

fn top(e: &testing::ElementHandle) -> f32 {
    e.absolute_position().y
}

fn bottom(e: &testing::ElementHandle) -> f32 {
    e.absolute_position().y + e.size().height
}

/// 新建行装得下它的输入框,输入框也不伸进下面的列表。
///
/// 行高写死 32px 时,56px 的 LineEdit 溢出 24px,列表首行和滚动条
/// 顶端都画在输入框底下。
#[test]
fn the_new_playlist_row_holds_its_input() {
    let ui = music_page(1000.0, 700.0, 1);
    let row = element(&ui, "MusicPage::new-playlist-row");
    let input = element(&ui, "MusicPage::new-name");
    let list = element(&ui, "MusicPage::playlist-list");

    assert!(
        row.size().height >= input.size().height,
        "行高 {} 装不下输入框 {}",
        row.size().height,
        input.size().height
    );
    assert!(
        bottom(&input) <= top(&list),
        "输入框下沿 {} 越过了列表上沿 {}",
        bottom(&input),
        top(&list)
    );
}
