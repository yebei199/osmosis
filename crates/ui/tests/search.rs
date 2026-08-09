//! 搜索三个页签的界面行为。无头跑,与 music_nav.rs 同一套路。
//!
//! `Tab::from_index` 证明的是**该搜什么**,这些 `if` 决定的是**用户看到什么**。
//! 后者写错的话,请求一路正确而屏幕上摆的是另一类结果。

use i_slint_backend_testing as testing;
use ui::MainWindow;

fn present(ui: &MainWindow, id: &str) -> bool {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .is_some()
}

/// 停在搜索分区的音乐页。登录之后才有 Music 页 —— 未登录时整块内容区都不建。
fn search_page() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.set_logged_in(true);
    ui.set_current_tab(1);
    ui.set_music_section(2);
    ui
}

/// 搜索分区默认停在「歌曲」页签。
#[test]
fn search_opens_on_the_tracks_tab() {
    let ui = search_page();

    assert_eq!(ui.get_search_tab(), 0, "默认该是歌曲");
    assert!(present(&ui, "MainWindow::track-list"));
    assert!(!present(&ui, "MainWindow::artist-list"));
}

/// 三个页签只在搜索分区里出现。
///
/// 别的分区没有关键词可搜,常驻一排页签会让人以为每日推荐也分歌手歌单。
#[test]
fn the_tabs_appear_only_in_the_search_section() {
    let ui = search_page();
    assert!(present(&ui, "MainWindow::search-tabs"));

    for section in [0, 1, 3] {
        ui.set_music_section(section);
        assert!(
            !present(&ui, "MainWindow::search-tabs"),
            "分区 {section} 不该有搜索页签"
        );
    }
}

/// 切到歌手页签,摆的是歌手列表而不是曲目列表。
///
/// 两个都摆的话,列表区会同时出现两批不相干的东西。
#[test]
fn the_artists_tab_shows_the_artist_list() {
    let ui = search_page();
    ui.set_search_tab(1);

    assert!(present(&ui, "MainWindow::artist-list"));
    assert!(
        !present(&ui, "MainWindow::track-list"),
        "歌手页签不该同时摆着一批歌"
    );
}

/// 切到歌单页签,摆的是歌单列表 —— 与「我的歌单」同一个组件、不同的一份数据。
#[test]
fn the_playlists_tab_shows_the_playlist_list() {
    let ui = search_page();
    ui.set_search_tab(2);

    assert!(present(
        &ui,
        "MainWindow::found-playlist-list"
    ));
    assert!(!present(&ui, "MainWindow::track-list"));
    // 「我的歌单」那张列表不在:两者各摆各的,共用一份数据的话,
    // 切回我的歌单会看见上一次的搜索结果
    assert!(!present(&ui, "MainWindow::playlist-list"));
}

/// 换关键词重搜,停在当前页签,不跳回歌曲。
///
/// 跳回去的话,想连搜三个歌手就得每次点两下。
#[test]
fn searching_again_stays_on_the_current_tab() {
    let ui = search_page();
    ui.set_search_tab(1);

    // 搜索本身要网络,这里只发回调:它不该顺手把页签拨回去
    ui.invoke_search("本兮".into());

    assert_eq!(ui.get_search_tab(), 1, "重搜不该换页签");
    assert!(present(&ui, "MainWindow::artist-list"));
}

/// 点开一位歌手之后,搜索框与页签让位给详情那一层。
///
/// 两层都在的话,返回键该收哪一层就说不清了 —— 与歌单详情同一条规矩。
#[test]
fn opening_an_artist_replaces_the_search_layer() {
    let ui = search_page();
    ui.set_search_tab(1);
    ui.set_open_playlist_name("本兮".into());

    assert!(present(&ui, "MainWindow::playlist-header"));
    assert!(present(&ui, "MainWindow::track-list"));
    assert!(!present(&ui, "MainWindow::artist-list"));
    assert!(!present(&ui, "MainWindow::search-tabs"));
    assert!(!present(&ui, "MainWindow::keyword"));
}
