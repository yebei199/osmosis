//! 断流横幅的界面行为。
//!
//! 无头跑:`i-slint-backend-testing` 给的是软件后端 + 模拟时钟,不要窗口、
//! 不要显卡,所以这条能进 `just ci`,不像截图那样只能在有合成器的机器上验。
//!
//! 这里补的是 Rust 侧纯函数够不着的那一半:`describe_stream_loss` 证明的是
//! **该说哪句话**,而 `.slint` 里那句 `if root.banner-text != ""` 决定的是
//! **用户到底看不看得见** —— 后者写错的话,前面整条链路一路正确,而屏幕上
//! 什么都没有(见 `docs/adr/0013`)。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use ui::MainWindow;
use ui::Shell;

/// 当前树里的横幅。`None` = 它压根不在,不是"在但空着"。
fn banner(
    ui: &MainWindow,
) -> Option<testing::ElementHandle> {
    testing::ElementHandle::find_by_element_id(
        ui,
        "MainWindow::banner",
    )
    .next()
}

/// **有话说才出现,话收了就消失。**
///
/// 两个方向都要钉:只钉"出现"的话,一个永远显示的横幅照样通过,
/// 而那意味着用户放好一首歌之后还挂着"没网了"。
#[test]
fn the_banner_shows_up_only_when_there_is_something_to_say()
{
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");

    assert!(
        banner(&ui).is_none(),
        "还没出事,横幅不该在树里"
    );

    ui.global::<Shell>()
        .set_banner_text("没网了,检查一下网络再试".into());
    assert!(
        banner(&ui).is_some(),
        "有话说了,横幅该出现 —— 文案对而看不见等于没说"
    );

    // 放成功了会走这一步(见 music.rs 的 play_current)。
    ui.global::<Shell>()
        .set_banner_text(slint::SharedString::new());
    assert!(
        banner(&ui).is_none(),
        "声音回来了,那句话已经过期,横幅该收掉"
    );
}
