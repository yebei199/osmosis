//! 下载的两个界面事实:长按菜单出不出现,以及进度那条带子出不出现。
//!
//! Rust 侧的纯函数证明的是**该说哪句话**(`music::download` 里那几条),
//! `.slint` 里那两个 `if` 决定的是**用户到底看不看得见** —— 后者写错的话,
//! 前面整条链路一路正确,而屏幕上什么都没有(与横幅同一条理由,见 banner.rs)。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use ui::Library;
use ui::MainWindow;
use ui::Shell;

/// 当前树里的长按菜单。`None` = 它压根不在。
fn menu(ui: &MainWindow) -> Option<testing::ElementHandle> {
    testing::ElementHandle::find_by_element_id(
        ui,
        "MainWindow::track-menu",
    )
    .next()
}

/// 菜单里那颗下载键,按无障碍标签找 —— 无头后端只认得动作,点不了裸 TouchArea。
fn download_button(
    ui: &MainWindow,
) -> Option<testing::ElementHandle> {
    testing::ElementHandle::find_by_accessible_label(
        ui, "下载",
    )
    .next()
}

/// 菜单**按住了才出现,收了就消失**。
///
/// 两个方向都要钉:只钉"出现"的话,一个永远挡在界面上的菜单照样通过 ——
/// 而它铺满整块、底下什么都点不动。
#[test]
fn the_track_menu_shows_up_only_for_a_track() {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");

    assert!(
        menu(&ui).is_none(),
        "没按住任何一行,菜单不该在树里"
    );

    ui.global::<Library>()
        .set_track_menu_id("1375305989".into());
    ui.global::<Library>()
        .set_track_menu_title("残響散歌".into());
    assert!(menu(&ui).is_some(), "按住了某一行,菜单该出现");

    ui.global::<Library>()
        .set_track_menu_id(slint::SharedString::new());
    assert!(
        menu(&ui).is_none(),
        "点别处之后菜单该收掉,否则界面就此卡住"
    );
}

/// 点下载:回调带着**那一行的 id**,并且菜单自己收掉。
///
/// id 传错的现象是下回来一首别的歌,而文件名还是对的 —— 没有任何一步会报错。
#[test]
fn the_download_button_reports_the_track_it_was_opened_for()
{
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");

    let asked =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let sink = asked.clone();
    ui.global::<Library>().on_download_track(move |id| {
        *sink.borrow_mut() = Some(id.to_string());
    });

    ui.global::<Library>()
        .set_track_menu_id("1375305989".into());
    ui.global::<Library>()
        .set_track_menu_title("残響散歌".into());

    download_button(&ui)
        .expect(
            "菜单里没有下载键 —— 读屏念不出的键等于没有",
        )
        .invoke_accessible_default_action();

    assert_eq!(
        asked.borrow().as_deref(),
        Some("1375305989"),
        "回调没带上按住的那一行"
    );
    assert!(
        menu(&ui).is_none(),
        "点完该收掉 —— 否则用户会对着菜单再点一次"
    );
}

/// 进度那条带子同样是**有话说才出现**。
///
/// 它是投影不是一次性提示:下载结束时由 Rust 清空(见 ui/src/music/download.rs),
/// 没有计时器会来替它收。
#[test]
fn the_progress_strip_follows_the_download_text() {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");

    let strip = || {
        testing::ElementHandle::find_by_element_id(
            &ui,
            "MainWindow::download-strip",
        )
        .next()
    };

    assert!(
        strip().is_none(),
        "没有下载在进行,带子不该在树里"
    );

    ui.global::<Shell>()
        .set_download_text("下载中 42%".into());
    assert!(
        strip().is_some(),
        "下载起来了,带子该出现 —— 进度算对而看不见等于没算"
    );

    ui.global::<Shell>()
        .set_download_text(slint::SharedString::new());
    assert!(strip().is_none(), "下载结束,带子该收掉");
}
