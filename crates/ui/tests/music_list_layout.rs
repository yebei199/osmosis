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

/// 设置与个人页的那一列在宽窗口里居中(MEO-1878),不贴左留下右半边空白。
fn assert_column_centered(dest: i32, id: &str) {
    let ui = music_page(1900.0, 900.0, dest);
    let column = element(&ui, id);
    let page_left = 168.0 + (1900.0 - 168.0 - 1400.0) / 2.0;
    let left_gap = column.absolute_position().x - page_left;
    let right_gap = page_left + 1400.0 - right(&column);
    assert!(
        (left_gap - right_gap).abs() < 1.0,
        "{id} 左 {left_gap} 右 {right_gap}"
    );
}

#[test]
fn settings_page_centers_its_column() {
    assert_column_centered(8, "SettingsPage::column");
}

#[test]
fn profile_page_centers_its_column() {
    assert_column_centered(7, "ProfilePage::column");
}

/// 列宽按 4:3:3 分,不随文字长短走:歌名歌手很长、专辑是「—」时,
/// 专辑列仍与歌手列一样宽。实测曾是歌手 228px、专辑 17px。
#[test]
fn wide_columns_follow_the_ratio_not_the_text() {
    let ui = music_page(1400.0, 900.0, 0);
    ui.global::<Shell>().set_wide(true);
    ui.global::<Player>().set_tracks(ModelRc::new(VecModel::from(vec![TrackRow {
        id: "1".into(),
        title: "一首名字非常非常长的歌,长到足够把整列都撑开还要再多出一截来".into(),
        artists: "歌手甲 / 歌手乙 / 歌手丙 / 歌手丁 / 歌手戊 / 歌手己".into(),
        album: "—".into(),
        duration: "3:41".into(),
        ..Default::default()
    }])));

    let album = element(&ui, "TrackList::album-cell");
    let artists = element(&ui, "TrackList::artists-cell");
    // 行要铺满列表:1232px 的内容区里,专辑那 3/10 至少两百多像素。
    // 实测曾经整行缩到文字最小宽,两列各 113px。
    assert!(
        album.size().width > 250.0,
        "专辑列只有 {}",
        album.size().width
    );
    assert!(
        (album.size().width - artists.size().width).abs()
            < 1.0,
        "歌手 {} 专辑 {}",
        artists.size().width,
        album.size().width
    );
}

/// 手机版式里列表在底栏与控制条之上收住(MEO-1878):最后一行的红心
/// 不被任何一条压住。量的是边,不是截图。
#[test]
fn compact_list_ends_above_the_bars() {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.window()
        .set_size(slint::LogicalSize::new(420.0, 900.0));
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_compact(true);
    ui.global::<Shell>().set_destination(6);
    let rows: Vec<TrackRow> = (0..40)
        .map(|i| TrackRow {
            id: i.to_string().into(),
            title: format!("第 {i} 首").into(),
            ..Default::default()
        })
        .collect();
    ui.global::<Player>()
        .set_tracks(ModelRc::new(VecModel::from(rows)));
    ui.global::<Player>().set_has_track(true);

    let list = element(&ui, "MusicPage::track-list");
    let nav = element(&ui, "MainWindow::bottom-bar");
    let bar = element(&ui, "PlayerBar::capsule");
    assert!(
        bottom(&list) <= top(&nav),
        "列表底 {} 底栏顶 {}",
        bottom(&list),
        top(&nav)
    );
    assert!(
        bottom(&list) <= top(&bar),
        "列表底 {} 控制条顶 {}",
        bottom(&list),
        top(&bar)
    );
}

/// 窗口先小后大(真实启动就是这样):列表跟着变宽,不卡在第一次排版的宽度上。
/// 实测曾在 1400 与 1900 下都停在 599px。
#[test]
fn track_list_follows_a_growing_window() {
    let ui = music_page(632.0, 900.0, 0);
    one_track(&ui);
    let narrow =
        element(&ui, "MusicPage::track-list").size().width;

    ui.window()
        .set_size(slint::LogicalSize::new(1400.0, 900.0));
    ui.global::<Shell>().set_wide(true);
    let wide =
        element(&ui, "MusicPage::track-list").size().width;
    assert!(wide > narrow + 500.0, "窄 {narrow} 宽 {wide}");
}

/// 分组条在场时列表照样铺满内容区(MEO-1878)。实测分组条那一排药丸的
/// 宽度把整列卡在 599px,1232px 的内容区右半边全空。
#[test]
fn the_facet_bar_does_not_cap_the_list_width() {
    let ui = music_page(1400.0, 900.0, 6);
    ui.global::<Shell>().set_wide(true);
    let labels: Vec<slint::SharedString> =
        ["不分组", "歌手", "专辑"].map(Into::into).to_vec();
    ui.global::<Player>().set_grouping_labels(
        ModelRc::new(VecModel::from(labels)),
    );
    ui.global::<Player>().set_facets_enabled(true);
    one_track(&ui);

    let list = element(&ui, "MusicPage::track-list");
    assert!(
        list.size().width > 1100.0,
        "列表只有 {}",
        list.size().width
    );
}
