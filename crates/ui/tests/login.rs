//! 登录页的界面行为。
//!
//! 无头跑,与 `banner.rs` 同一套路:`i-slint-backend-testing` 给软件后端 +
//! 模拟时钟,不要窗口也不要显卡。
//!
//! 这里钉的是 Rust 侧纯函数够不着的一半:`login_failure_text` 证明的是
//! **该说哪句话**,而 `.slint` 里那两个 `if` 决定的是**用户能不能绕过登录** ——
//! 后者写错的话,人点进音乐页只会收获一屏 401,而那看起来像是后端坏了。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use ui::MainWindow;
use ui::Session;

fn find(
    ui: &MainWindow,
    id: &str,
) -> Option<testing::ElementHandle> {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
}

/// **没登录才出现,登录了就消失。**
///
/// 两个方向都要钉:只钉"出现"的话,一个永远盖着的登录页照样通过,
/// 而那意味着登录成功之后还挡在那里。
#[test]
fn the_login_page_is_shown_only_when_logged_out() {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");

    ui.global::<Session>().set_logged_in(false);
    assert!(
        find(&ui, "MainWindow::login-page").is_some(),
        "没登录时登录页该在树里"
    );

    ui.global::<Session>().set_logged_in(true);
    assert!(
        find(&ui, "MainWindow::login-page").is_none(),
        "登录之后登录页该收掉,不能继续挡着"
    );
}

/// **提交键按无障碍标签找得到、点得动。**
///
/// 曾经它是唯一不走 HoverButton `text` 属性、自己塞子 Text 的调用点,
/// 标签因此为空:读屏念不出它,真机驱动只能从截图上量坐标点
/// (AGENTS.md「真机上的应用登录」记的就是这次绕行)。
#[test]
fn the_submit_button_answers_to_its_label() {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(false);

    let button =
        testing::ElementHandle::find_by_accessible_label(
            &ui, "登录",
        )
        .find(|h| {
            h.accessible_role()
                == Some(testing::AccessibleRole::Button)
        })
        .expect("登录键该报出自己的标签");

    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = asked.clone();
    ui.global::<Session>().on_login(move |_, _| {
        counter.set(counter.get() + 1);
    });
    button.invoke_accessible_default_action();
    assert_eq!(asked.get(), 1, "点登录键该喊一声 login");
}

/// 登录页盖着时导航不在树里。
///
/// 用 `if` 而不是 `visible: false` 正是为了这一条:后者只是看不见,
/// 元素仍在、仍能被点到,用户就能绕过登录点进音乐页。
#[test]
fn the_nav_is_unreachable_while_logged_out() {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");

    ui.global::<Session>().set_logged_in(false);
    assert!(
        find(&ui, "MainWindow::content").is_none(),
        "没登录时内容区不该在树里 —— 绕过登录点进去只会收获一屏 401"
    );

    ui.global::<Session>().set_logged_in(true);
    assert!(
        find(&ui, "MainWindow::content").is_some(),
        "登录之后内容区该回来"
    );
}
