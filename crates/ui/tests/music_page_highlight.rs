//! 音乐页 TrackList 里,正在播的那一行要能被认出来(#114)。无头跑。
//!
//! 颜色与描边读不到(测试框架不导出通用属性读取),能读的是 `accessible-checked`
//! —— 与 controls.slint 的芯片选中态同一套约定,既是给读屏用的真实信号,
//! 也是这里唯一够得着的断言点。

use i_slint_backend_testing as testing;
use slint::ComponentHandle as _;
use slint::{ModelRc, VecModel};
use ui::Player;
use ui::Session;
use ui::Shell;
use ui::{MainWindow, TrackRow};

fn row(id: &str) -> TrackRow {
    TrackRow {
        id: id.into(),
        title: format!("曲{id}").into(),
        artists: "人".into(),
        duration: "03:00".into(),
        loading: false,
        liked: false,
        cover_url: String::new().into(),
        cover: slint::Image::default(),
    }
}

fn music_page_with(rows: Vec<TrackRow>) -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Player>()
        .set_tracks(ModelRc::new(VecModel::from(rows)));
    ui
}

fn checked_flags(ui: &MainWindow) -> Vec<bool> {
    testing::ElementHandle::find_by_element_id(
        ui,
        "TrackList::touch",
    )
    .map(|el| el.accessible_checked().unwrap_or(false))
    .collect()
}

/// 点了第二首之后,只有它那一行被标成正在播,其余不动。
#[test]
fn the_playing_row_is_marked_current() {
    let ui =
        music_page_with(vec![row("1"), row("2"), row("3")]);

    ui.global::<Player>().set_now_id("2".into());

    assert_eq!(
        checked_flags(&ui),
        vec![false, true, false],
        "只有 id 匹配 now-id 的那一行该被标记"
    );
}

/// 切歌之后,高亮跟着挪到新的那一行,不留在旧的上面。
#[test]
fn the_highlight_follows_track_switches() {
    let ui =
        music_page_with(vec![row("1"), row("2"), row("3")]);

    ui.global::<Player>().set_now_id("1".into());
    assert_eq!(
        checked_flags(&ui),
        vec![true, false, false]
    );

    ui.global::<Player>().set_now_id("3".into());
    assert_eq!(
        checked_flags(&ui),
        vec![false, false, true],
        "切到第三首后,高亮该从第一行挪到第三行"
    );
}

/// 没有正在播的歌时(`now-id` 空串),没有任何一行被标记。
#[test]
fn no_row_is_marked_when_nothing_is_playing() {
    let ui = music_page_with(vec![row("1"), row("2")]);

    assert_eq!(checked_flags(&ui), vec![false, false]);
}
