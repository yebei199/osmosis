//! 音乐页列表层的几何(#130)。无头跑,量的是布局给出的真实位置与尺寸。
//!
//! 这几条毛病肉眼看是「压住了」「多一条缝」「标题全是…」,落到数上都是
//! 某个元素的边没对上另一个元素的边 —— 所以断言的是边,不是截图。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use slint::{ModelRc, VecModel};
use ui::{
    Library, MainWindow, Player, Session, Shell, TrackRow,
};

/// 宽版式音乐页,窗口给定逻辑尺寸。版式直接给定:无头后端里宽度推不出它
/// (见 music_nav.rs)。
fn music_page(
    width: f32,
    height: f32,
    section: i32,
) -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.window()
        .set_size(slint::LogicalSize::new(width, height));
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_compact(false);
    ui.global::<Shell>().set_destination(section);
    ui
}

fn one_track(ui: &MainWindow) {
    ui.global::<Player>().set_tracks(ModelRc::new(
        VecModel::from(vec![TrackRow {
            id: "1".into(),
            title: "甜甜的".into(),
            artists: "本兮".into(),
            album: "Hello World".into(),
            duration: "3:41".into(),
            loading: false,
            liked: false,
            cover_url: String::new().into(),
            cover: slint::Image::default(),
            header: false,
            collapsed: false,
        }]),
    ));
}

fn element(
    ui: &MainWindow,
    id: &str,
) -> testing::ElementHandle {
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

fn right(e: &testing::ElementHandle) -> f32 {
    e.absolute_position().x + e.size().width
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

/// 非每日推荐分区,列表一直铺到竖栏底 —— 中间没有卡墙那格空占位
/// 多吃的 12px 间距。
#[test]
fn the_list_layer_reaches_the_bottom_outside_the_daily_section()
 {
    let ui = music_page(1000.0, 700.0, 1);
    // 侧栏竖条(music-rail)随一级导航移出了音乐页(MEO-1876),量尺换成页面本身:
    // 没有控制条时页底留常规页边距 16px。
    let page =
        testing::ElementHandle::find_by_element_type_name(
            &ui,
            "MusicPage",
        )
        .next()
        .expect("音乐页该在");
    let floor = bottom(&page) - 16.0;
    let list = element(&ui, "MusicPage::playlist-list");
    assert_eq!(
        bottom(&list),
        floor,
        "歌单列表底下多了一条缝"
    );

    ui.global::<Shell>().set_destination(3);
    one_track(&ui);
    let list = element(&ui, "MusicPage::track-list");
    assert_eq!(
        bottom(&list),
        floor,
        "最近播放列表底下多了一条缝"
    );
}

/// 手机横屏(小米 13 横放约 873×393)打开本地歌单,曲目列表占满内容列宽。
///
/// 「把刚才那批加进来」那颗键没给宽度时取 HoverButton 的默认 220px,
/// 定宽的一个子元素把整列的宽度上限锁在 220,曲目标题被挤成「…」。
#[test]
fn the_track_list_spans_the_column_in_landscape() {
    let ui = music_page(873.0, 393.0, 1);
    ui.global::<Library>()
        .set_open_playlist_name("睡前".into());
    ui.global::<Library>().set_open_playlist_local(true);
    ui.global::<Library>().set_add_batch_text(
        "把刚才那批 20 首加进来".into(),
    );
    one_track(&ui);

    let content = element(&ui, "MainWindow::content");
    let list = element(&ui, "MusicPage::track-list");
    // 页面右 padding 16px,列表的右边就是内容区右边往里 16px。
    assert_eq!(
        right(&list),
        right(&content) - 16.0,
        "曲目列表宽 {},没铺满内容列",
        list.size().width
    );
}

/// 宽屏(MEO-1878):歌手与专辑各占一列,专辑格写的是专辑名。
#[test]
fn wide_layout_splits_the_row_into_columns() {
    let ui = music_page(1400.0, 900.0, 0);
    ui.global::<Shell>().set_wide(true);
    one_track(&ui);

    let album = element(&ui, "TrackList::album-cell");
    let artists = element(&ui, "TrackList::artists-cell");
    assert!(
        album.size().width > 100.0,
        "专辑列太窄:{}",
        album.size().width
    );
    assert!(
        artists.absolute_position().x
            < album.absolute_position().x,
        "歌手列该在专辑列左边"
    );
}

/// 中等宽度没有多列,仍是歌名上、歌手下。
#[test]
fn medium_layout_keeps_two_lines() {
    let ui = music_page(800.0, 900.0, 0);
    ui.global::<Shell>().set_wide(false);
    one_track(&ui);

    assert!(
        testing::ElementHandle::find_by_element_id(
            &ui,
            "TrackList::album-cell"
        )
        .next()
        .is_none()
    );
}

/// 内容区不超过 1400px,居中摆在侧栏右边的空间里。
#[test]
fn music_page_is_capped_and_centered() {
    let ui = music_page(1900.0, 900.0, 0);
    ui.global::<Shell>().set_wide(true);
    one_track(&ui);

    let page = element(&ui, "MainWindow::music-page");
    assert!(
        (page.size().width - 1400.0).abs() < 0.5,
        "宽 {}",
        page.size().width
    );
    let left_gap = page.absolute_position().x - 168.0;
    let right_gap = 1900.0 - right(&page);
    assert!(
        (left_gap - right_gap).abs() < 1.0,
        "左 {left_gap} 右 {right_gap}"
    );
}
