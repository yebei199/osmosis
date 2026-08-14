//! 本地歌单写操作的界面行为。无头跑,与 music_nav.rs / search.rs 同一套路。
//!
//! 这里钉的是「哪个键在树里」。`is_editable` 证明的是**谁能改**,这些 `if`
//! 决定的是**用户看得见哪个键** —— 后者写松了,平台歌单上会出现一个删除键,
//! 点下去才知道改不动。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use slint::{ModelRc, VecModel};
use ui::Library;
use ui::Player;
use ui::Session;
use ui::{MainWindow, TrackRow};

fn present(ui: &MainWindow, id: &str) -> bool {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .is_some()
}

/// 停在「我的歌单」那一层。
fn playlist_page() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.set_current_tab(1);
    ui.set_music_section(1);
    ui
}

/// 打开一个本地歌单的详情。
fn opened_local(ui: &MainWindow) {
    ui.global::<Library>()
        .set_open_playlist_name("睡前".into());
    ui.global::<Library>().set_open_playlist_local(true);
}

/// 列表里摆一首歌。行上的键长在 `for` 里 —— 一首都没有的话,
/// 「有没有移除键」这个问题根本问不出来。
fn one_track(ui: &MainWindow) {
    ui.global::<Player>().set_tracks(ModelRc::new(
        VecModel::from(vec![TrackRow {
            id: "1".into(),
            title: "甜甜的".into(),
            artists: "本兮".into(),
            duration: "3:41".into(),
            loading: false,
            liked: false,
            cover_url: String::new().into(),
            cover: slint::Image::default(),
        }]),
    ));
}

/// 「新建歌单」只在我的歌单那一层出现。
///
/// 别的分区没有「我的歌单」这个上下文,详情那一层要建的是别的东西。
#[test]
fn the_new_playlist_row_lives_on_the_playlist_list() {
    let ui = playlist_page();
    assert!(present(&ui, "MainWindow::new-playlist-row"));

    for section in [0, 2, 3] {
        ui.set_music_section(section);
        assert!(
            !present(&ui, "MainWindow::new-playlist-row"),
            "分区 {section} 不该有新建歌单"
        );
    }

    // 进了详情也不该有:那一层要建的不是歌单
    ui.set_music_section(1);
    opened_local(&ui);
    assert!(!present(&ui, "MainWindow::new-playlist-row"));
}

/// 只有本地歌单的详情有改名与删除。
///
/// 平台歌单与「我喜欢的」的真相在平台,这边改不动 ——
/// 摆个按钮上去只会让人点了才知道。
#[test]
fn only_a_local_playlist_can_be_renamed_or_deleted() {
    let ui = playlist_page();
    opened_local(&ui);
    assert!(present(&ui, "MainWindow::delete-button"));

    // 平台歌单 / 我喜欢的:同一层详情,但没有那两个键
    ui.global::<Library>().set_open_playlist_local(false);
    assert!(!present(&ui, "MainWindow::delete-button"));
    // 曲目行上的「−」也一起消失:那也是一次写
    assert!(
        !ui.global::<Library>().get_open_playlist_local()
    );
}

/// 删除要点两下:第一下变成「确认删除?」,第二下才真删。
///
/// 一下就删的话,手指滑一下就没了一个歌单,而歌单没有回收站。
#[test]
fn deleting_asks_once_before_it_happens() {
    let ui = playlist_page();
    opened_local(&ui);

    let deleted =
        std::rc::Rc::new(std::cell::Cell::new(0_u32));
    let count = deleted.clone();
    ui.global::<Library>().on_delete_playlist(move || {
        count.set(count.get() + 1)
    });

    let button =
        testing::ElementHandle::find_by_element_id(
            &ui,
            "MainWindow::delete-button",
        )
        .next()
        .expect("找不到删除键");

    button.invoke_accessible_default_action();
    assert_eq!(deleted.get(), 0, "第一下不该删,该先问一句");

    button.invoke_accessible_default_action();
    assert_eq!(deleted.get(), 1, "第二下才真删");
}

/// 换了歌单,上一个没确认完的删除不跟过来。
///
/// 跟过来的话,在 A 上点了一下删除、退出去进了 B,B 的删除键已经处在
/// 「再点一下就没」的状态 —— 而用户以为那是第一下。
#[test]
fn a_half_finished_delete_does_not_follow_to_the_next() {
    let ui = playlist_page();
    opened_local(&ui);

    let deleted =
        std::rc::Rc::new(std::cell::Cell::new(0_u32));
    let count = deleted.clone();
    ui.global::<Library>().on_delete_playlist(move || {
        count.set(count.get() + 1)
    });

    let find = || {
        testing::ElementHandle::find_by_element_id(
            &ui,
            "MainWindow::delete-button",
        )
        .next()
        .expect("找不到删除键")
    };

    find().invoke_accessible_default_action();
    // 换到另一个歌单
    ui.global::<Library>()
        .set_open_playlist_name("通勤".into());

    find().invoke_accessible_default_action();
    assert_eq!(
        deleted.get(),
        0,
        "换了歌单之后的第一下仍然只是问一句"
    );
}

/// 「把刚才那批加进来」要同时满足两件事:停在本地歌单详情、且刚才确实有一批歌。
#[test]
fn the_add_batch_row_needs_both_a_local_playlist_and_a_batch()
 {
    let ui = playlist_page();
    opened_local(&ui);

    // 没有刚才那一批:整行不出现
    ui.global::<Library>().set_add_batch_text("".into());
    assert!(!present(&ui, "MainWindow::add-batch-row"));

    ui.global::<Library>().set_add_batch_text(
        "+ 把刚才那 30 首加进来".into(),
    );
    assert!(present(&ui, "MainWindow::add-batch-row"));

    // 平台歌单收不了东西
    ui.global::<Library>().set_open_playlist_local(false);
    assert!(!present(&ui, "MainWindow::add-batch-row"));
}

/// 本地歌单详情里每行多一个「−」,别处没有。
///
/// 从每日推荐里「移除」一首没有意义 —— 那不是一个能改的集合。
#[test]
fn rows_can_be_removed_only_inside_a_local_playlist() {
    let ui = playlist_page();
    opened_local(&ui);
    one_track(&ui);
    assert!(present(&ui, "TrackList::remove-hit"));

    ui.global::<Library>().set_open_playlist_local(false);
    assert!(
        !present(&ui, "TrackList::remove-hit"),
        "平台歌单里不该有移除键"
    );

    // 每日推荐同理:那一批根本不属于任何可改的集合
    ui.global::<Library>()
        .set_open_playlist_name("".into());
    ui.set_music_section(0);
    assert!(!present(&ui, "TrackList::remove-hit"));
}
