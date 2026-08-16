//! 统一播放条 PlayerBar 的界面行为(#81)。无头跑,与 controls.rs 同一套路。
//!
//! 这一组管的是「条在不在、摆得对不对、翻得动翻不动」;
//! 进度算得对不对仍由 progress::ratio 那边证。

use i_slint_backend_testing as testing;
use slint::ComponentHandle;
use ui::MainWindow;
use ui::Player;
use ui::Session;
use ui::Shell;

/// 按无障碍标签找键。那一条上的圆键都是同一个 `RoundControl`,
/// 元素 id 分不开它们,而标签本来就是为了让人分得开才有的。
fn key(
    ui: &MainWindow,
    label: &str,
) -> Option<testing::ElementHandle> {
    testing::ElementHandle::find_by_accessible_label(ui, label)
        .next()
}

fn present(ui: &MainWindow, id: &str) -> bool {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .is_some()
}

/// 登录、有歌、宽版式。条要在这三样都齐了才摆得出来。
fn playing_app() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_compact(false);
    ui.global::<Player>().set_has_track(true);
    ui
}

/// 非音乐页也有播放条:首页、个人主页、设置页在有歌时都摆迷你胶囊。
/// 改版前这三页里只有首页有,另两页整个没有 —— 换页就丢失控制入口。
#[test]
fn the_mini_bar_shows_on_every_non_music_page() {
    let ui = playing_app();

    for tab in [0, 2, 3] {
        ui.global::<Shell>().set_current_tab(tab);
        assert!(
            present(&ui, "PlayerBar::capsule"),
            "tab {tab} 上该有迷你播放条"
        );
    }
}

/// 没歌时哪一页都不摆条。空壳会让人以为点它能开始放,而那时没有「开始」可言。
#[test]
fn no_bar_anywhere_without_a_track() {
    let ui = playing_app();
    ui.global::<Player>().set_has_track(false);

    for tab in [0, 1, 2, 3] {
        ui.global::<Shell>().set_current_tab(tab);
        assert!(
            !present(&ui, "PlayerBar::capsule"),
            "tab {tab} 上没歌不该摆条"
        );
    }
}

/// 迷你条点非控件区就地展开成完整条,再点收回。
/// 「就地」是关键:展开不该把人踢去音乐页,当前页的上下文要留着。
#[test]
fn tapping_the_mini_bar_expands_it_in_place() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(2);

    // 迷你形态没有切歌键,那是完整条面 A 才有的。
    assert!(key(&ui, "上一首").is_none(), "迷你形态不该有上一首");

    key(&ui, "展开播放条")
        .expect("找不到展开手势")
        .invoke_accessible_default_action();

    assert!(
        key(&ui, "上一首").is_some(),
        "展开后该露出面 A 的切歌键"
    );
    assert_eq!(
        ui.global::<Shell>().get_current_tab(),
        2,
        "展开是就地的,不该换页"
    );

    key(&ui, "收起播放条")
        .expect("找不到收起手势")
        .invoke_accessible_default_action();
    assert!(key(&ui, "上一首").is_none(), "收回后该退回迷你形态");
}

/// 迷你条上的环形键仍然只切播放,不触发展开 ——
/// 控件区的点击归控件,别被底下那层展开手势吃掉。
#[test]
fn the_mini_ring_toggles_playback_without_expanding() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(2);
    ui.global::<Player>().set_is_playing(false);

    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = asked.clone();
    ui.global::<Player>().on_toggle_play(move || {
        counter.set(counter.get() + 1);
    });

    key(&ui, "播放")
        .expect("迷你条上找不到播放键")
        .invoke_accessible_default_action();

    assert_eq!(asked.get(), 1, "按一下该喊一声");
    assert!(
        key(&ui, "上一首").is_none(),
        "按播放键不该顺手把条展开"
    );
}

/// 面 A 是默认面:播放键与上下曲在场,模式键与同播不在。
#[test]
fn face_a_carries_playback_and_nothing_else() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_bar_face(0);

    assert!(key(&ui, "上一首").is_some());
    assert!(key(&ui, "下一首").is_some());
    assert!(key(&ui, "播放").is_some());
    assert!(
        key(&ui, "随机播放").is_none(),
        "随机键归面 B,不该出现在面 A"
    );
    assert!(
        !present(&ui, "SyncStrip::sync-empty"),
        "同播归面 C,不该出现在面 A"
    );
}

/// 面 B 才有随机、循环与音量;此时面 A 的切歌键退场。
/// 三面轮换的意义就在这里:每面只摆自己那一簇。
#[test]
fn face_b_carries_the_modes_and_volume() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_bar_face(1);

    assert!(key(&ui, "随机播放").is_some());
    assert!(key(&ui, "循环: 关").is_some());
    assert!(
        present(&ui, "VolumeControl::speaker"),
        "音量键归面 B"
    );
    assert!(
        key(&ui, "上一首").is_none(),
        "切歌键归面 A,翻走了就该退场"
    );
}

/// 面 C 才有同播区。设备为空时那句说明仍常驻 —— 同播不能因为没设备就消失。
#[test]
fn face_c_carries_the_sync_strip() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_bar_face(2);

    assert!(
        present(&ui, "SyncStrip::sync-empty"),
        "一台设备都没有时那句说明该常驻"
    );
    assert!(
        key(&ui, "随机播放").is_none(),
        "模式键归面 B,翻走了就该退场"
    );
}

/// 三颗指示点各自直达一面,点哪面到哪面,不必挨个轮。
#[test]
fn the_face_dots_jump_straight_to_their_face() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_bar_face(0);

    key(&ui, "翻到同播")
        .expect("找不到同播那颗指示点")
        .invoke_accessible_default_action();
    assert_eq!(
        ui.global::<Shell>().get_bar_face(),
        2,
        "点同播那颗该直接到面 C,不该只走一格"
    );

    key(&ui, "翻到播放")
        .expect("找不到播放那颗指示点")
        .invoke_accessible_default_action();
    assert_eq!(ui.global::<Shell>().get_bar_face(), 0);
}

/// 进度轨跨三面常驻:翻到模式面、同播面时,拖动落点的能力不丢。
/// 这是 ADR 0010「功能永远有等价路径」在翻面结构上的落点。
#[test]
fn the_progress_track_stays_across_all_faces() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);

    for face in [0, 1, 2] {
        ui.global::<Shell>().set_bar_face(face);
        assert!(
            present(&ui, "PlayerBar::track"),
            "面 {face} 上进度轨该还在"
        );
    }
}

/// 紧凑版式里进度轨不靠悬停现形。手机没有 hover,
/// 一条只在指针悬停时才出现的轨道在触屏上等于不存在。
#[test]
fn the_progress_track_is_always_visible_in_compact() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_compact(true);

    let track =
        testing::ElementHandle::find_by_element_id(
            &ui,
            "PlayerBar::track",
        )
        .next()
        .expect("紧凑版式里找不到进度轨");
    assert!(
        track.size().height > 0.0,
        "紧凑版式里进度轨该占着实高,而不是等悬停才现形"
    );
}

/// 条身把尺寸回写给背景通道,Rust 侧按这个尺寸渲(沿用 #68 的 bar-w/h)。
#[test]
fn the_bar_mirrors_its_size_for_the_backdrop() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);

    // 条件页面要先查一次元素逼出实例化,init 才会跑。
    let _ = key(&ui, "播放");

    assert!(
        ui.global::<Shell>().get_bar_w() > 0.0,
        "条身该把宽度回写,实得 {}",
        ui.global::<Shell>().get_bar_w()
    );
    assert!(
        ui.global::<Shell>().get_bar_h() > 0.0,
        "条身该把高度回写"
    );
}

/// 播放页用的是同一个条,不再是另一套控制簇。
/// 展开/收起播放页共用面 A 上那颗方向键,图标随场景翻个个儿。
#[test]
fn the_play_page_uses_the_same_bar() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_bar_face(0);

    assert!(
        key(&ui, "展开播放页").is_some(),
        "音乐页那颗方向键该是展开"
    );

    ui.global::<Shell>().set_play_page_open(true);
    assert!(
        key(&ui, "收起播放页").is_some(),
        "播放页上同一颗键该翻成收起"
    );
    assert!(
        ui.global::<Shell>().get_viz_bar_w() > 0.0,
        "播放页的条该把尺寸回写给覆层那一槽"
    );
}

/// 紧凑版式里方向键仍在屏内。钉 #68 那次挤爆回归:
/// 一行塞不下时按钮会被推出屏外,而那是播放页唯一的出入口。
#[test]
fn the_expand_button_stays_on_screen_in_compact() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_compact(true);
    ui.global::<Shell>().set_bar_face(0);

    let expand =
        key(&ui, "展开播放页").expect("紧凑版式里找不到方向键");
    let right =
        expand.absolute_position().x + expand.size().width;
    let window_w = ui.window().size().width as f32
        / ui.window().scale_factor();
    assert!(
        right <= window_w,
        "方向键右缘 {right} 超出窗宽 {window_w}"
    );
}
