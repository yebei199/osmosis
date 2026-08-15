use slint::{Model, ModelRc, VecModel};

use super::*;
use crate::PlaylistRow;
use crate::playlist::Source;

/// 一个摆好歌单列表的无头窗口。
fn window_with(rows: Vec<PlaylistRow>) -> MainWindow {
    i_slint_backend_testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Library>()
        .set_playlists(ModelRc::new(VecModel::from(rows)));
    ui
}

fn playlist_row(
    name: &str,
    source: Source,
    subtitle: &str,
) -> PlaylistRow {
    PlaylistRow {
        id: name.into(),
        name: name.into(),
        subtitle: subtitle.into(),
        source: source.to_index(),
        cover: slint::Image::default(),
    }
}

/// 某一行现在的副标题。
fn subtitle_of(ui: &MainWindow, index: usize) -> String {
    ui.global::<Library>()
        .get_playlists()
        .row_data(index)
        .expect("这一行该在")
        .subtitle
        .to_string()
}

fn set_of(ids: &[&str]) -> LikedSet {
    Rc::new(RefCell::new(
        ids.iter().map(|id| (*id).to_owned()).collect(),
    ))
}

/// 在集合里的算红心,不在的不算。
#[test]
fn rows_are_marked_against_the_liked_set() {
    let set = set_of(&["1", "3"]);

    assert!(is_liked(&set, "1"));
    assert!(is_liked(&set, "3"));
    assert!(!is_liked(&set, "2"));
}

/// 点红心当场把「我喜欢的 N 首」加一。
///
/// 那个数字来自 `/playlists`,而点红心的成功路径不重拉它 —— 现象是心变红了、
/// 旁边的数字还是旧的,要切出歌单分区再回来才变。
#[test]
fn liking_a_track_bumps_the_liked_count() {
    let ui = window_with(vec![playlist_row(
        "我喜欢的",
        Source::Liked,
        "976 首",
    )]);

    bump_liked_count(&ui, 1);

    assert_eq!(subtitle_of(&ui, 0), "977 首");
}

/// 取消红心当场减一。
#[test]
fn unliking_a_track_drops_the_liked_count() {
    let ui = window_with(vec![playlist_row(
        "我喜欢的",
        Source::Liked,
        "976 首",
    )]);

    bump_liked_count(&ui, -1);

    assert_eq!(subtitle_of(&ui, 0), "975 首");
}

/// 只动「我喜欢的」那一行。别的歌单的条数与这次点击无关,
/// 动了它们就是凭空改数据。
#[test]
fn other_playlists_keep_their_counts() {
    let ui = window_with(vec![
        playlist_row("我喜欢的", Source::Liked, "976 首"),
        playlist_row("anime", Source::Platform, "152 首"),
        playlist_row("rap", Source::Local, "7 首"),
    ]);

    bump_liked_count(&ui, 1);

    assert_eq!(subtitle_of(&ui, 0), "977 首");
    assert_eq!(
        subtitle_of(&ui, 1),
        "152 首",
        "平台歌单的条数与这次点击无关"
    );
    assert_eq!(
        subtitle_of(&ui, 2),
        "7 首",
        "本地歌单的条数与这次点击无关"
    );
}

/// 边界:读不出条数的那一行不动它(比如「暂无曲目」)。
///
/// 猜一个数字写上去比留着旧的更糟 —— 旧的至少下次拉 `/playlists` 会自愈,
/// 猜的那个会一直看起来很正常。
#[test]
fn a_row_without_a_readable_count_is_left_alone() {
    let ui = window_with(vec![playlist_row(
        "我喜欢的",
        Source::Liked,
        "暂无曲目",
    )]);

    bump_liked_count(&ui, 1);

    assert_eq!(
        subtitle_of(&ui, 0),
        "暂无曲目",
        "读不出条数就不动它 —— 猜一个写上去会一直看起来很正常"
    );
}

/// 点下去立刻改本地集合 —— 等服务端来回一趟才变色的话,
/// 手指底下没有任何反馈,人会连点。
#[test]
fn toggling_updates_the_set_immediately() {
    let set = set_of(&[]);

    assert!(
        set_liked(&set, "1", true),
        "本来没有,加上算改动"
    );
    assert!(is_liked(&set, "1"));

    assert!(
        set_liked(&set, "1", false),
        "本来有,去掉算改动"
    );
    assert!(!is_liked(&set, "1"));
}

/// 重复点同一个方向不算改动 —— 回滚要靠这个返回值,
/// 没改动的话撤回也无从撤起。
#[test]
fn a_no_op_toggle_reports_no_change() {
    let set = set_of(&["1"]);

    assert!(!set_liked(&set, "1", true), "已经红心了");
    assert!(!set_liked(&set, "2", false), "本来就没红心");
}

/// 撤回把集合还原成点之前的样子。
///
/// 留着一个假的红心,下次进来就变回去了,而用户以为自己点成功了。
#[test]
fn a_failed_toggle_rolls_back() {
    let set = set_of(&["1"]);

    // 用户点了「取消红心」
    set_liked(&set, "1", false);
    assert!(!is_liked(&set, "1"));

    // 请求失败,撤回
    set_liked(&set, "1", true);
    assert!(
        is_liked(&set, "1"),
        "撤回之后该回到点之前的样子"
    );
}
