//! 歌词页(#73)的界面行为。无头跑。
//!
//! 「现在唱到第几行」归 `app_core::current_line`,「该画哪几行」归
//! `app_core::window`,两者各有自己的单测。这里钉的是页面这一半:
//! 进得去、退得出、景深摆得对、开关只在有数据时出现、拖动不碰播放。

use i_slint_backend_testing as testing;
use slint::platform::PointerEventButton;
use slint::{
    ComponentHandle, LogicalPosition, ModelRc, VecModel,
};
use ui::{LyricRow, MainWindow};

/// 播放页展开、有歌词的窗口。
///
/// 登录页盖住整个窗口,它的表单控件会吃掉落在下面的指针事件,所以先登录。
fn play_window() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.window()
        .set_size(slint::LogicalSize::new(400.0, 800.0));
    ui.set_logged_in(true);
    ui.set_play_page_open(true);
    ui.set_has_track(true);
    ui.set_now_title("歌词测试曲".into());
    ui.set_lyric_line("当前这一行".into());
    ui
}

/// 把歌词页展开并让滑出动画走完 —— 无头跑没有时间流逝,
/// 不推时钟的话整页仍停在屏幕外,点不着也拖不动。
fn open_lyrics(ui: &MainWindow) {
    ui.set_lyrics_page_open(true);
    testing::mock_elapsed_time(
        std::time::Duration::from_millis(400),
    );
}

/// 焦点行在中间的一窗歌词。`translated` 决定译文有没有数据。
fn rows(translated: bool) -> ModelRc<LyricRow> {
    let rows: Vec<LyricRow> = (-3..=3)
        .map(|offset| LyricRow {
            text: format!("第 {offset} 行").into(),
            translation: if translated {
                format!("译 {offset}").into()
            } else {
                Default::default()
            },
            offset,
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

fn element(
    ui: &MainWindow,
    id: &str,
) -> Option<testing::ElementHandle> {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
}

fn all(
    ui: &MainWindow,
    id: &str,
) -> Vec<testing::ElementHandle> {
    testing::ElementHandle::find_by_element_id(ui, id)
        .collect()
}

/// 播放页上那块歌词,它同时是歌词页的入口。
fn lyric_area(ui: &MainWindow) -> testing::ElementHandle {
    element(ui, "MainWindow::lyric-entry")
        .expect("播放页该有歌词入口")
}

/// 点播放页的歌词区域,歌词页展开。
#[test]
fn tapping_the_lyric_area_opens_the_lyrics_page() {
    let ui = play_window();
    ui.set_lyric_rows(rows(false));
    assert!(!ui.get_lyrics_page_open(), "初始该是收着的");

    lyric_area(&ui)
        .mock_single_click(PointerEventButton::Left);

    assert!(
        ui.get_lyrics_page_open(),
        "点歌词该展开歌词页"
    );
}

/// 取不到歌词时那块区域根本不在,也就点不出一页空行。
#[test]
fn there_is_no_entry_without_lyrics() {
    let ui = play_window();
    ui.set_lyric_line("".into());

    assert!(
        element(&ui, "MainWindow::lyric-entry").is_none(),
        "没歌词不该留入口"
    );
}

/// 点非歌词区收回,播放页原样还在。
#[test]
fn tapping_outside_the_lines_returns_to_the_play_page() {
    let ui = play_window();
    ui.set_lyric_rows(rows(false));
    open_lyrics(&ui);

    element(&ui, "LyricsPage::backdrop")
        .expect("歌词页该有可点的空白区")
        .mock_single_click(PointerEventButton::Left);

    assert!(
        !ui.get_lyrics_page_open(),
        "点空白该收回歌词页"
    );
    assert!(
        ui.get_play_page_open(),
        "收回歌词页不该连播放页一起收"
    );
}

/// 景深:焦点行最亮最大,每远一行透明度与高度单调递减。
///
/// 递减不单调的话视线找不到「现在在哪」,景深就成了噪声。
#[test]
fn opacity_and_size_fall_off_with_line_distance() {
    let ui = play_window();
    ui.set_lyric_rows(rows(false));
    open_lyrics(&ui);

    let lines = all(&ui, "LyricsPage::line");
    assert_eq!(lines.len(), 7, "该画出一整窗七行");

    // 行按 offset -3..=3 顺序排,中间那条是焦点行。
    let focus = 3;
    for step in 1..=3usize {
        for near_index in
            [focus - step + 1, focus + step - 1]
        {
            let near = &lines[near_index];
            for far_index in [focus - step, focus + step] {
                let far = &lines[far_index];
                assert!(
                    far.computed_opacity()
                        < near.computed_opacity(),
                    "第 {far_index} 行该比第 {near_index} 行淡:{} vs {}",
                    far.computed_opacity(),
                    near.computed_opacity()
                );
                assert!(
                    far.size().height < near.size().height,
                    "第 {far_index} 行该比第 {near_index} 行矮:{} vs {}",
                    far.size().height,
                    near.size().height
                );
            }
        }
    }
}

/// 翻译开关只在有译文时存在;关掉后译文行不实例化。
#[test]
fn the_translation_toggle_exists_only_with_translation_data()
 {
    let ui = play_window();
    open_lyrics(&ui);

    ui.set_lyric_rows(rows(false));
    ui.set_lyric_has_translation(false);
    assert!(
        element(&ui, "LyricsPage::translation-toggle")
            .is_none(),
        "没译文不该留开关"
    );
    assert!(
        all(&ui, "LyricsPage::translation").is_empty(),
        "没译文不该画译文行"
    );

    ui.set_lyric_rows(rows(true));
    ui.set_lyric_has_translation(true);
    let toggle =
        element(&ui, "LyricsPage::translation-toggle")
            .expect("有译文该有开关");
    assert_eq!(
        all(&ui, "LyricsPage::translation").len(),
        7,
        "开着时每行都该有译文"
    );

    toggle.mock_single_click(PointerEventButton::Left);
    assert!(
        all(&ui, "LyricsPage::translation").is_empty(),
        "关掉后译文行该不实例化"
    );
}

/// 歌词页打开时世界空间标注卡不在元素树里 —— 点云已退成背景光,
/// 卡片再浮着就是标一个看不见的物体。
#[test]
fn the_annotation_card_hides_while_lyrics_are_open() {
    let ui = play_window();
    ui.set_viz_anchor_visible(true);
    assert!(
        element(&ui, "MainWindow::viz-anchor-card")
            .is_some(),
        "播放页该有标注卡"
    );

    ui.set_lyrics_page_open(true);
    assert!(
        element(&ui, "MainWindow::viz-anchor-card")
            .is_none(),
        "歌词页打开后标注卡该让位"
    );
}

/// 拖动改变的是浏览偏移,不碰播放进度。
#[test]
fn dragging_browses_without_touching_playback() {
    let ui = play_window();
    ui.set_lyric_rows(rows(false));
    open_lyrics(&ui);
    ui.set_progress_ratio(0.3);

    let sought = std::rc::Rc::new(std::cell::Cell::new(0));
    let sink = sought.clone();
    ui.on_seek(move |_| sink.set(sink.get() + 1));

    let area = element(&ui, "LyricsPage::backdrop")
        .expect("该有可拖的区域");
    let origin = area.absolute_position();
    let size = area.size();

    // 往上拖:内容跟着上移,窗口向后面的行走。
    area.mock_drag(
        LogicalPosition::new(
            origin.x + size.width / 2.0,
            origin.y + size.height / 2.0 - 200.0,
        ),
        PointerEventButton::Left,
    );

    assert!(
        ui.get_lyric_browse() > 0,
        "上拖该把窗口推向后面的行,实测 {}",
        ui.get_lyric_browse()
    );
    assert_eq!(sought.get(), 0, "拖歌词不该 seek");
    assert!(
        (ui.get_progress_ratio() - 0.3).abs()
            < f32::EPSILON,
        "拖歌词不该动进度"
    );
}

/// 歌词页打开时,播放页的歌名与那两行歌词一并让位。
///
/// 它们与歌词页画在同一片区域,留着就是两层字叠在一起(小米13 真机实拍)。
#[test]
fn the_play_page_text_steps_aside_for_the_lyrics_page() {
    let ui = play_window();
    ui.set_lyric_rows(rows(false));
    assert!(
        element(&ui, "MainWindow::lyric-entry").is_some(),
        "播放页该有那两行歌词"
    );

    open_lyrics(&ui);

    assert!(
        element(&ui, "MainWindow::lyric-entry").is_none(),
        "歌词页打开后播放页那两行该让位"
    );
    assert!(
        element(&ui, "MainWindow::play-title").is_none(),
        "歌名也该让位"
    );
}
