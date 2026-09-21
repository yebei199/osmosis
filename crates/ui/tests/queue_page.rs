//! 播放页上那层队列覆层(#109 AC-15)的界面行为。无头跑。
//!
//! 「哪一批、第几条」归 `music::queuepage` 与 `app_core::Queue`,各有自己的
//! 单测。这里钉的是页面这一半:进得去、退得出、点一行报的是 `entry_id`,
//! 以及**列表的下缘不许钻到控制条底下**。
//!
//! 最后那一条在截图上几乎看不出来:被盖住的是列表末尾几行,而一份长队列
//! 本来就要滚到底才看得见它们。

use i_slint_backend_testing as testing;
use slint::{ComponentHandle, ModelRc, VecModel};
use ui::Player;
use ui::Session;
use ui::Shell;
use ui::Viz;
use ui::{MainWindow, TrackRow};

/// 播放页展开、队列里有歌的窗口。
///
/// 登录页盖住整个窗口,它的表单控件会吃掉落在下面的指针事件,所以先登录。
fn queue_window(rows: usize) -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.window()
        .set_size(slint::LogicalSize::new(400.0, 800.0));
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_play_page_open(true);
    ui.global::<Player>().set_has_track(true);
    ui.global::<Viz>().set_now_title("队列测试曲".into());

    let rows: Vec<TrackRow> = (0..rows)
        .map(|at| TrackRow {
            // 队列页里这个字段装的是 `entry_id`,不是曲目 id。
            id: format!("{}", 100 + at).into(),
            title: format!("第 {at} 首").into(),
            artists: "某人".into(),
            duration: "3:00".into(),
            loading: false,
            liked: false,
            cover_url: Default::default(),
            cover: Default::default(),
        })
        .collect();
    ui.global::<Viz>().set_queue_total(rows.len() as i32);
    ui.global::<Viz>()
        .set_queue_rows(ModelRc::new(VecModel::from(rows)));
    ui
}

/// 把队列页滑出来并让动画走完 —— 无头跑没有时间流逝,不推时钟的话
/// 整页仍停在屏幕外,点不着也量不着。
fn open_queue(ui: &MainWindow) {
    ui.global::<Viz>().set_queue_page_open(true);
    testing::mock_elapsed_time(
        std::time::Duration::from_millis(400),
    );
}

fn one(
    ui: &MainWindow,
    id: &str,
) -> testing::ElementHandle {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .unwrap_or_else(|| panic!("界面里没有 {id}"))
}

/// 关着的时候整页停在屏幕右外侧,开了才滑进来。
///
/// 它与歌词页不一样:歌词页外面套着 `if rows.length > 0`,不满足就根本
/// 不建;队列页是常驻的,只靠 `x` 挪出视野。`x` 那条绑定写反的话,它会
/// 一直盖在播放页上,而播放页看起来就是"打不开了"。
#[test]
fn the_page_waits_off_screen_until_it_is_opened() {
    let ui = queue_window(8);
    let width = 400.0;

    let closed = one(&ui, "QueuePage::queue-list");
    assert!(
        closed.absolute_position().x >= width,
        "关着的时候列表在 x={},该在窗口外(>= {width})",
        closed.absolute_position().x
    );

    open_queue(&ui);

    let opened = one(&ui, "QueuePage::queue-list");
    assert!(
        opened.absolute_position().x < width,
        "开了之后该滑进来,实际 x={}",
        opened.absolute_position().x
    );
    assert!(opened.size().height > 0.0);
}

/// 空队列与「正在取」分开说,不是同一句话。
///
/// 长得一样的话,用户会以为自己的歌没了。
#[test]
fn an_empty_queue_and_a_loading_one_read_differently() {
    let ui = queue_window(0);
    open_queue(&ui);

    let when_empty = one(&ui, "QueuePage::queue-empty")
        .accessible_label()
        .expect("空状态那行该有可读文本");

    ui.global::<Viz>().set_queue_loading(true);
    let when_loading = one(&ui, "QueuePage::queue-empty")
        .accessible_label()
        .expect("取数中那行该有可读文本");

    assert_ne!(
        when_empty, when_loading,
        "「空的」与「正在取」说的是同一句话"
    );
}

/// **列表的下缘不许钻到控制条底下。**
///
/// 控制簇按 `docs/adr/0010` 永远排在覆层之后、压在最上层,所以队列页
/// 自己要把那一段让出来。让不出来时被盖住的是列表**末尾**几行 —— 而一份
/// 长队列本来就要滚到底才看得见它们,截图上看不出来,真机上更看不出来。
///
/// 手机上底下还要再多让一条手势条(`safe-bottom`),桌面上那个数恒 0 ——
/// 所以这条在桌面上量到的是两者里更宽松的那一半。
#[test]
fn the_list_stops_above_the_player_bar() {
    let ui = queue_window(40);
    open_queue(&ui);

    let list = one(&ui, "QueuePage::queue-list");
    let bar = one(&ui, "PlayPage::player-bar");

    let list_bottom =
        list.absolute_position().y + list.size().height;
    let bar_top = bar.absolute_position().y;

    assert!(
        list_bottom <= bar_top,
        "列表下缘 {list_bottom} 钻到了控制条上缘 {bar_top} 底下,\
         末尾 {} 逻辑像素的行是点不着的",
        list_bottom - bar_top
    );
}

/// 点一行报出去的是那一行的 `entry_id`,不是下标、也不是曲目 id。
///
/// 队列允许同一首歌出现多次,按曲目或下标认的话,点第二次出现的那一条
/// 会放到第一条上去。
#[test]
fn picking_a_row_reports_its_entry_id() {
    let ui = queue_window(6);
    open_queue(&ui);

    let picked =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::<
            String,
        >::new(
        )));
    let sink = std::rc::Rc::clone(&picked);
    ui.global::<Viz>().on_queue_pick(move |entry| {
        sink.borrow_mut().push(entry.to_string());
    });

    // 第三行:`queue_window` 给的号是 100 + 下标。
    let rows: Vec<testing::ElementHandle> =
        testing::ElementHandle::find_by_element_id(
            &ui,
            "TrackList::touch",
        )
        .collect();
    assert!(rows.len() >= 3, "只找到 {} 行", rows.len());
    rows[2].mock_single_click(
        slint::platform::PointerEventButton::Left,
    );

    assert_eq!(picked.borrow().as_slice(), ["102"]);
}

/// 队列里有歌、页还关着的时候,那颗入口药丸必须在。
///
/// 它是打开这一页的**唯一**入口。2026-09-21 真机上撞到过一次死结:
/// Rust 那侧「页没开就整轮不算账」把 `queue-total` 一起跳过了,于是
/// 总数恒为 0、药丸永远不出现、这一页永远打不开 —— 而单测、桌面截图
/// 全都看不出来,因为它们都是直接把 `queue-page-open` 设成 true 进去的。
#[test]
fn the_pill_opens_the_page_while_it_is_still_closed() {
    let ui = queue_window(6);
    // 播放页自己也是滑进来的:不推时钟的话它还在半路上,点在哪里都不算。
    testing::mock_elapsed_time(
        std::time::Duration::from_millis(400),
    );

    assert!(
        one(&ui, "PlayPage::queue-entry").size().width
            > 0.0
    );

    let pill = one(&ui, "PlayPage::queue-entry-touch");
    pill.mock_single_click(
        slint::platform::PointerEventButton::Left,
    );

    assert!(
        ui.global::<Viz>().get_queue_page_open(),
        "点了药丸,队列页该开"
    );
}
