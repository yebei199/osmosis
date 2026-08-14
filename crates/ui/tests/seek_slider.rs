//! 播放页右侧竖向进度滑条(#75)的界面行为。无头跑,与 controls.rs 同一套路。
//!
//! `progress::ratio` 已证明算得对,这里管的是滑条**在不在、画得对、拖了喊谁**。
//! 竖条从顶往下填:时间线读下来,与滚动条同一个心智模型。

use i_slint_backend_testing as testing;
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, LogicalPosition};
use ui::MainWindow;

/// 播放页展开、手上有一首歌的窗口。滑条是播放页里的条件元素。
fn play_window() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.window()
        .set_size(slint::LogicalSize::new(400.0, 800.0));
    // 登录页盖住整个窗口,它的表单控件会吃掉落在滑条上的指针事件。
    ui.set_logged_in(true);
    ui.set_play_page_open(true);
    ui.set_has_track(true);
    ui.set_now_title("滑条测试曲".into());
    ui
}

fn element(
    ui: &MainWindow,
    id: &str,
) -> Option<testing::ElementHandle> {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
}

fn slider(
    ui: &MainWindow,
) -> Option<testing::ElementHandle> {
    element(ui, "MainWindow::seek-slider")
}

/// 滑条里那条填充。它的高度就是「放到哪儿了」的全部视觉证据。
fn fill(ui: &MainWindow) -> testing::ElementHandle {
    element(ui, "VerticalSeekBar::fill")
        .expect("滑条该有填充块")
}

fn groove(ui: &MainWindow) -> testing::ElementHandle {
    element(ui, "VerticalSeekBar::groove")
        .expect("滑条该有轨道")
}

/// 滑条只属于播放页:页没展开、或没有曲目时不该实例化。
///
/// 没曲目时留一条空滑条,拖它会 seek 到一个不存在的位置。
#[test]
fn the_slider_exists_only_on_the_play_page_with_a_track() {
    let ui = play_window();
    assert!(
        slider(&ui).is_some(),
        "播放页有曲目时该有滑条"
    );

    ui.set_has_track(false);
    assert!(slider(&ui).is_none(), "没曲目不该留滑条");

    ui.set_has_track(true);
    ui.set_play_page_open(false);
    assert!(
        slider(&ui).is_none(),
        "播放页收起后滑条该跟着走"
    );
}

/// 填充高度随 progress-ratio 单调增长:0 时不占高,1 时占满轨道。
#[test]
fn the_fill_grows_with_the_progress_ratio() {
    let ui = play_window();
    let track = groove(&ui).size().height;
    assert!(track > 0.0, "轨道该有高度");

    ui.set_progress_ratio(0.0);
    let empty = fill(&ui).size().height;
    ui.set_progress_ratio(0.5);
    let half = fill(&ui).size().height;
    ui.set_progress_ratio(1.0);
    let full = fill(&ui).size().height;

    assert!(
        empty < half && half < full,
        "填充该随比例单调增长,实测 {empty} / {half} / {full}"
    );
    assert!(
        empty < 1.0,
        "比例 0 时填充该收干净,实测 {empty}"
    );
    assert!(
        (full - track).abs() < 1.0,
        "比例 1 时该填满轨道 {track},实测 {full}"
    );
    assert!(
        (half - track / 2.0).abs() < 2.0,
        "比例 0.5 该在轨道中点,实测 {half}"
    );
}

/// 顶端是开头:比例小的时候填充贴在轨道**上**半截。
///
/// 填反了整条滑条的读数都是镜像的,而填充高度那条断言对翻转毫无察觉。
#[test]
fn the_fill_starts_at_the_top() {
    let ui = play_window();
    ui.set_progress_ratio(0.25);

    let fill_top = fill(&ui).absolute_position().y;
    let groove_top = groove(&ui).absolute_position().y;

    assert!(
        (fill_top - groove_top).abs() < 1.0,
        "填充该从轨道顶端起算:轨道 {groove_top},填充 {fill_top}"
    );
}

/// 拖动只喊 seek(比例),不自己改 progress-ratio ——
/// 位置的真相在播放器,界面先斩后奏会在 seek 失败时留下一个骗人的进度。
#[test]
fn dragging_shouts_seek_without_setting_the_ratio() {
    let ui = play_window();
    ui.set_progress_ratio(0.1);

    let heard = std::rc::Rc::new(std::cell::RefCell::new(
        Vec::<f32>::new(),
    ));
    let sink = heard.clone();
    ui.on_seek(move |at| sink.borrow_mut().push(at));

    let track = groove(&ui);
    let top = track.absolute_position().y;
    let height = track.size().height;
    let x = track.absolute_position().x
        + track.size().width / 2.0;

    // 拖到轨道的四分之三处。起点是元素中心(即 0.5),所以这一拖是真的动了。
    slider(&ui).expect("该有滑条").mock_drag(
        LogicalPosition::new(x, top + height * 0.75),
        PointerEventButton::Left,
    );

    let calls = heard.borrow();
    assert_eq!(
        calls.len(),
        1,
        "一次拖动该只在松手时喊一次 seek,实测 {calls:?}"
    );
    assert!(
        (calls[0] - 0.75).abs() < 0.05,
        "该 seek 到 0.75,实测 {}",
        calls[0]
    );
    assert!(
        (ui.get_progress_ratio() - 0.1).abs()
            < f32::EPSILON,
        "界面不该自己置位,实测 {}",
        ui.get_progress_ratio()
    );
}

/// 滑条报得出自己的位置,读屏也用得上它。
/// 一条只能用手指拖的进度条,对键盘与读屏用户等于不存在。
#[test]
fn the_slider_reports_its_position_to_assistive_tech() {
    let ui = play_window();
    ui.set_progress_ratio(0.4);

    let handle = slider(&ui).expect("该有滑条");
    assert_eq!(
        handle.accessible_role(),
        Some(testing::AccessibleRole::Slider),
        "滑条该报 Slider 角色"
    );
    let value: f32 = handle
        .accessible_value()
        .expect("该报得出当前值")
        .parse()
        .expect("值该是个数");
    assert!(
        (value - 0.4).abs() < 0.01,
        "报出的值该跟着比例走,实测 {value}"
    );

    let heard =
        std::rc::Rc::new(std::cell::Cell::new(f32::NAN));
    let sink = heard.clone();
    ui.on_seek(move |at| sink.set(at));
    handle.set_accessible_value("0.8");
    assert!(
        (heard.get() - 0.8).abs() < 0.01,
        "读屏改值该走同一条 seek,实测 {}",
        heard.get()
    );
}

/// 时间读数迁到滑条上,并在缓冲时换成「缓冲中…」——
/// 缓冲期间位置本来就不动,让一个僵住的数字待着最像卡死。
#[test]
fn the_slider_carries_the_time_readout() {
    let ui = play_window();
    ui.set_progress_text("1:23 / 3:41".into());

    let handle = slider(&ui).expect("该有滑条");
    assert_eq!(
        handle.accessible_label().as_deref(),
        Some("1:23 / 3:41"),
        "滑条该报时间读数"
    );

    ui.set_buffering(true);
    assert_eq!(
        handle.accessible_label().as_deref(),
        Some("缓冲中…"),
        "缓冲时该换成缓冲文案"
    );
}

/// 时间读数要能完整显示。它的框只有滑条那 44px 宽的话,
/// 「1:23 / 3:46」会被裁成「1:23 /」(小米13 真机实拍)。
#[test]
fn the_time_readout_is_wider_than_the_slider() {
    let ui = play_window();
    ui.set_progress_text("1:23 / 3:46".into());

    let readout = element(&ui, "MainWindow::play-time-readout")
        .expect("播放页该有时间读数");
    let bar = slider(&ui).expect("该有滑条");

    assert!(
        readout.size().width > bar.size().width * 1.5,
        "读数框该比滑条宽出一截:滑条 {},读数 {}",
        bar.size().width,
        readout.size().width
    );
}
