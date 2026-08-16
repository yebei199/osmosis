//! 控制条:主条 + 抽屉(#84/#85/#86)。无头跑,与 controls.rs 同一套路。
//!
//! 这一组管的是「一根条到处一样、抽屉装得下、开关看得出」。
//! 进度算得对不对仍由 progress::ratio 那边证。

use i_slint_backend_testing as testing;
use slint::ComponentHandle;
use ui::MainWindow;
use ui::Player;
use ui::Session;
use ui::Shell;

/// 除播放页外的四个页签。播放页是覆层,由 `play-page-open` 单开。
const PAGE_TABS: [i32; 4] = [0, 1, 2, 3];

fn key(
    ui: &MainWindow,
    label: &str,
) -> Option<testing::ElementHandle> {
    testing::ElementHandle::find_by_accessible_label(ui, label)
        .next()
}

fn ids(
    ui: &MainWindow,
    id: &str,
) -> Vec<testing::ElementHandle> {
    testing::ElementHandle::find_by_element_id(ui, id).collect()
}

fn present(ui: &MainWindow, id: &str) -> bool {
    !ids(ui, id).is_empty()
}

/// 把窗口调到手机那么窄。**只拨 `compact` 那一位是不够的** ——
/// 它只说明「该用紧凑版式」,窗口宽度不跟着变,条照样是宽的,
/// 于是所有关于让位的断言都会假通过。
fn narrow(ui: &MainWindow) {
    ui.window().set_size(slint::LogicalSize::new(360.0, 780.0));
    ui.global::<Shell>().set_compact(true);
}

/// 登录、有歌、宽版式。条要这三样齐了才摆得出来。
fn playing_app() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_compact(false);
    ui.global::<Player>().set_has_track(true);
    ui
}

/// 条身那颗胶囊此刻的几何。没有条时返回 None。
fn bar_box(
    ui: &MainWindow,
) -> Option<(f32, f32, f32, f32)> {
    let h = ids(ui, "PlayerBar::capsule").into_iter().next()?;
    let p = h.absolute_position();
    let s = h.size();
    Some((p.x, p.y, s.width, s.height))
}

/// 按无障碍标签取一颗键的直径(宽)。
fn key_width(ui: &MainWindow, label: &str) -> f32 {
    key(ui, label)
        .unwrap_or_else(|| panic!("找不到「{label}」"))
        .size()
        .width
}

/// 环形播放键的直径。它的标签随播放态在「播放 / 暂停」之间换,
/// 任一时刻只有一个在场 —— 探针得认这一点,不能两个都要。
fn ring_width(ui: &MainWindow) -> f32 {
    key(ui, "播放")
        .or_else(|| key(ui, "暂停"))
        .expect("找不到环形播放键")
        .size()
        .width
}

/// 往窗口上某一点真按一下,走命中测试。
///
/// **这是 `invoke_accessible_default_action` 做不到的事。** 那个是对着元素直接调,
/// 盖在它上面的东西一概不参与;而用户的手指得穿过那一层。凡是「点得到吗」的断言
/// 都要走这条路 —— 一层透明的 TouchArea 压在上面时,只有这条会红。
fn click_point(ui: &MainWindow, x: f32, y: f32) {
    use slint::platform::PointerEventButton;
    use slint::platform::WindowEvent;

    let position = slint::LogicalPosition::new(x, y);
    let button = PointerEventButton::Left;
    let win = ui.window();
    win.dispatch_event(WindowEvent::PointerMoved { position });
    win.dispatch_event(WindowEvent::PointerPressed {
        position,
        button,
    });
    win.dispatch_event(WindowEvent::PointerReleased {
        position,
        button,
    });
}

/// 往某个元素的正中真按一下。
fn click_at(ui: &MainWindow, h: &testing::ElementHandle) {
    let p = h.absolute_position();
    let s = h.size();
    click_point(
        ui,
        p.x + s.width / 2.0,
        p.y + s.height / 2.0,
    );
}

/// 抽屉此刻的样子:高度,加上逐行的无障碍标签。
/// 抽屉开合是每根条各记各的,所以这里自己先开一次。
fn drawer_shape(ui: &MainWindow) -> (f32, Vec<String>) {
    key(ui, "更多")
        .expect("找不到抽屉键")
        .invoke_accessible_default_action();
    let h = ids(ui, "PlayerBar::drawer")
        .into_iter()
        .next()
        .expect("抽屉该开着")
        .size()
        .height;
    let rows = ids(ui, "DrawerRow::touch")
        .iter()
        .filter_map(|r| {
            r.accessible_label().map(|s| s.to_string())
        })
        .collect();
    (h, rows)
}

// ============ 一根条到处一样(#85)============

/// 五处页面都摆同一根条:首页、个人主页、设置、音乐页,以及播放页覆层。
/// 改版前非音乐页那颗是另一套内容(少上一曲、少时间),换页就换了根条。
#[test]
fn every_page_carries_the_same_bar() {
    let ui = playing_app();

    for tab in PAGE_TABS {
        ui.global::<Shell>().set_current_tab(tab);
        assert!(
            present(&ui, "PlayerBar::capsule"),
            "tab {tab} 上该有控制条"
        );
    }

    ui.global::<Shell>().set_play_page_open(true);
    assert!(
        ids(&ui, "PlayerBar::capsule").len() >= 1,
        "播放页覆层上也该有同一根条"
    );
}

/// 条高、圆环直径、按键直径、封面尺寸在所有页面上完全相同。
/// 此前是六档条高、五档圆环 —— 这条把「不统一」钉成可回归的数字。
#[test]
fn the_bar_geometry_is_identical_everywhere() {
    let ui = playing_app();
    let mut seen: Vec<(i32, f32, f32, f32, f32)> = vec![];

    for tab in PAGE_TABS {
        ui.global::<Shell>().set_current_tab(tab);
        let (_, _, _, h) =
            bar_box(&ui).expect("该有条");
        seen.push((
            tab,
            h,
            ring_width(&ui),
            key_width(&ui, "上一首"),
            key_width(&ui, "下一首"),
        ));
    }

    let first = seen[0];
    for row in &seen[1..] {
        assert_eq!(
            (row.1, row.2, row.3, row.4),
            (first.1, first.2, first.3, first.4),
            "tab {} 的条高/环径/键径与 tab {} 不一致:{row:?} vs {first:?}",
            row.0,
            first.0
        );
    }
}

/// 条身左右内缩在所有调用点相同。此前三处 16 / 24 / 32px,切页时边沿在跳。
#[test]
fn the_bar_inset_is_identical_everywhere() {
    let ui = playing_app();
    let window_w = ui.window().size().width as f32
        / ui.window().scale_factor();
    let mut insets: Vec<(i32, f32)> = vec![];

    for tab in PAGE_TABS {
        ui.global::<Shell>().set_current_tab(tab);
        let (x, _, w, _) = bar_box(&ui).expect("该有条");
        // 右内缩;左内缩由居中保证与它相等。
        insets.push((tab, window_w - (x + w)));
    }

    let first = insets[0].1;
    for (tab, inset) in &insets[1..] {
        assert!(
            (inset - first).abs() < 0.5,
            "tab {tab} 的内缩 {inset} 与 tab {} 的 {first} 不一致",
            insets[0].0
        );
    }
}

/// 每一页都能切上一首。改版前迷你形态压根没有这颗键,
/// 在设置页想切回上一首是做不到的。
#[test]
fn previous_track_is_reachable_from_every_page() {
    let ui = playing_app();

    for tab in PAGE_TABS {
        ui.global::<Shell>().set_current_tab(tab);
        assert!(
            key(&ui, "上一首").is_some(),
            "tab {tab} 上该能切上一首"
        );
    }
}

/// 窄容器下条不溢出,让位的是曲名那一列而不是按键。
/// 钉 #68 那次挤爆回归:一行塞不下时按钮会被推出屏外。
#[test]
fn a_narrow_container_shrinks_the_title_not_the_keys() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    narrow(&ui);

    let window_w = ui.window().size().width as f32
        / ui.window().scale_factor();
    for label in ["上一首", "下一首", "更多"] {
        let k = key(&ui, label)
            .unwrap_or_else(|| panic!("找不到「{label}」"));
        let right =
            k.absolute_position().x + k.size().width;
        assert!(
            right <= window_w,
            "紧凑版式里「{label}」右缘 {right} 超出窗宽 {window_w}"
        );
    }
}

/// 窄到只放得下一样时,留下的是曲名而不是时间读数。
/// 位置本来就有进度轨在报,而「在放哪首」没有第二个出处。
/// 两次实拍才试出这个顺序:时间跟歌手串一行会被末尾吃掉,
/// 给时间一个不让位的槽又会把曲名压成一个省略号。
#[test]
fn a_narrow_bar_keeps_the_title_and_drops_the_clock() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    narrow(&ui);

    let title = ids(&ui, "PlayerBar::title")
        .into_iter()
        .next()
        .expect("找不到曲名");
    assert!(
        title.size().width > 60.0,
        "窄档下曲名该还剩得下几个字,实得 {}",
        title.size().width
    );
    assert!(
        !present(&ui, "PlayerBar::clock"),
        "窄到放不下时时间该整格收起,而不是留个被切断的读数"
    );
}

/// 宽到放得下时,时间读数在场。
#[test]
fn a_wide_bar_shows_the_clock() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    assert!(
        present(&ui, "PlayerBar::clock"),
        "宽版式下该看得见时间读数"
    );
}

/// 没歌时哪一页都不摆条。空壳会让人以为点它能开始放。
#[test]
fn no_bar_anywhere_without_a_track() {
    let ui = playing_app();
    ui.global::<Player>().set_has_track(false);

    for tab in PAGE_TABS {
        ui.global::<Shell>().set_current_tab(tab);
        assert!(
            !present(&ui, "PlayerBar::capsule"),
            "tab {tab} 上没歌不该摆条"
        );
    }
}

/// 三面轮换退场:面号、指示点、翻面手势都不该还在。
/// 留着任何一个,就还有第二种「条现在长什么样」。
#[test]
fn the_three_face_rotation_is_gone() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);

    for label in ["翻到播放", "翻到模式", "翻到同播"] {
        assert!(
            key(&ui, label).is_none(),
            "指示点「{label}」该随三面轮换一起退场"
        );
    }
    assert!(
        !present(&ui, "PlayerBar::flip-touch"),
        "翻面手势该退场"
    );
}

// ============ 封面兼任播放页进出口(#85)============

/// 条外点封面展开播放页,播放页里点封面收起。一个入口管两个方向,
/// 专用的 ▲/▼ 键随之退场。
#[test]
fn the_cover_toggles_the_play_page() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);

    // 钉到封面元素本身:此前有一颗专用箭头键也叫这个标签,
    // 只按标签找会匹配到它,测不出「封面兼任入口」。
    assert!(
        present(&ui, "PlayerBar::cover"),
        "封面该是个具名元素,它就是播放页的进出口"
    );
    let cover = ids(&ui, "PlayerBar::cover")
        .into_iter()
        .next()
        .expect("找不到封面");
    assert_eq!(
        cover.accessible_label().as_deref(),
        Some("展开播放页"),
        "条外时封面该报成展开播放页"
    );
    cover.invoke_accessible_default_action();
    assert!(
        ui.global::<Shell>().get_play_page_open(),
        "点封面该展开播放页"
    );

    let cover = ids(&ui, "PlayerBar::cover")
        .into_iter()
        .next()
        .expect("找不到封面");
    assert_eq!(
        cover.accessible_label().as_deref(),
        Some("收起播放页"),
        "播放页里同一个封面该翻成收起"
    );
    cover.invoke_accessible_default_action();
    assert!(
        !ui.global::<Shell>().get_play_page_open(),
        "再点该收起"
    );
}

/// 播放页上只剩一套 seek 面。此前条上的横轨与右缘竖向滑条并存,
/// 两套槽宽、颜色、把手规则,而只有竖的那条报 slider 角色。
#[test]
fn the_play_page_has_exactly_one_seek_surface() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_play_page_open(true);

    assert!(
        !present(&ui, "PlayPage::seek-slider"),
        "竖向进度条该退场,seek 只归条上那道轨"
    );
}

/// 时间读数全应用只剩一份。播放页此前在条上和右缘各显示一次,
/// 两种字体两种颜色。
#[test]
fn the_time_readout_appears_once() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_play_page_open(true);

    assert!(
        !present(&ui, "PlayPage::play-time-readout"),
        "播放页右缘那份时间读数该退场"
    );
}

// ============ 抽屉(#86)============

/// 抽屉默认收起,点抽屉键才展开,再点条外收起。
#[test]
fn the_drawer_starts_closed_and_toggles() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);

    assert!(
        !present(&ui, "PlayerBar::drawer"),
        "抽屉默认该收着"
    );

    key(&ui, "更多")
        .expect("找不到抽屉键")
        .invoke_accessible_default_action();
    assert!(
        present(&ui, "PlayerBar::drawer"),
        "点抽屉键该展开"
    );

    key(&ui, "收起更多")
        .expect("展开后该有收起的入口")
        .invoke_accessible_default_action();
    assert!(
        !present(&ui, "PlayerBar::drawer"),
        "再点该收起"
    );
}

/// 随机、循环、音量、同播都住在抽屉里,收起时不占主条。
#[test]
fn the_drawer_holds_the_modes_and_sync() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);

    assert!(
        key(&ui, "随机播放").is_none(),
        "收起时随机不该占主条"
    );
    assert!(
        !present(&ui, "VolumeControl::slider"),
        "收起时音量滑块不该占主条"
    );

    key(&ui, "更多")
        .expect("找不到抽屉键")
        .invoke_accessible_default_action();

    assert!(key(&ui, "随机播放").is_some());
    assert!(key(&ui, "循环: 关").is_some());
    assert!(
        present(&ui, "VolumeControl::slider"),
        "音量滑块在抽屉里常驻,不必再点开一层"
    );
    assert!(
        present(&ui, "SyncStrip::sync-empty"),
        "同播区在抽屉里,一台设备都没有时那句说明也常驻"
    );
}

/// 抽屉向**上**长:开合前后主条纹丝不动,而抽屉整个落在主条上方。
/// 播放页那根曾经被一层 HorizontalLayout 把高度锁在 84px —— 条身自己算出的
/// 262px 用不上,于是抽屉从条的位置往下铺,盖住主条、越过窗口下缘。
/// 只量抽屉自己的尺寸是看不出来的:它高度对、行数对,就是长错了方向。
#[test]
fn the_drawer_grows_upward_and_never_moves_the_bar() {
    let ui = playing_app();

    for tab in PAGE_TABS {
        ui.global::<Shell>().set_current_tab(tab);
        check_drawer_grows_upward(&ui, &format!("tab {tab}"));
    }

    ui.global::<Shell>().set_play_page_open(true);
    check_drawer_grows_upward(&ui, "播放页");
}

/// 在当前这一页上开一次抽屉,验条不动、抽屉在条上方。
fn check_drawer_grows_upward(ui: &MainWindow, page: &str) {
    let before = bar_box(ui).expect("该有条");
    key(ui, "更多")
        .expect("找不到抽屉键")
        .invoke_accessible_default_action();

    let after = bar_box(ui).expect("开着抽屉时条该还在");
    assert_eq!(
        after, before,
        "{page}:开抽屉不该动主条"
    );

    let drawer = ids(ui, "PlayerBar::drawer")
        .into_iter()
        .next()
        .expect("抽屉该开着");
    let bottom = drawer.absolute_position().y
        + drawer.size().height;
    assert!(
        bottom <= after.1,
        "{page}:抽屉下缘 {bottom} 压到了主条上缘 {}",
        after.1
    );

    key(ui, "收起更多")
        .expect("该有收起的入口")
        .invoke_accessible_default_action();
}

/// 抽屉在播放页里与在别处一模一样:同样高、同样几行。
/// 一根条到处一样,展开出来的那一层自然也该到处一样 —— 此前播放页多一行
/// 视觉预设,于是「展开」在两处不是一件事。
#[test]
fn the_drawer_is_identical_everywhere() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    let outside = drawer_shape(&ui);

    ui.global::<Shell>().set_play_page_open(true);
    let inside = drawer_shape(&ui);

    assert_eq!(
        inside, outside,
        "播放页的抽屉与别处不一致"
    );
}

/// 抽屉里那几行要真的点得到。
///
/// 收起用的那块全屏 TouchArea 曾经声明在抽屉**之后** —— slint 里后声明即在上层,
/// 于是它压住整根条,每一次点击都被吃成「收起抽屉」,随机、循环、音量一个都拨不动。
/// 上面那批用无障碍动作的断言全部照过:那条路不过命中测试。
#[test]
fn a_real_click_reaches_the_drawer_rows() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    key(&ui, "更多")
        .expect("找不到抽屉键")
        .invoke_accessible_default_action();

    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = asked.clone();
    ui.global::<Player>().on_shuffle_toggled(move || {
        counter.set(counter.get() + 1);
    });

    let row = key(&ui, "随机播放").expect("找不到随机");
    click_at(&ui, &row);

    assert_eq!(
        asked.get(),
        1,
        "点在随机那一行上就该拨随机"
    );
    assert!(
        present(&ui, "PlayerBar::drawer"),
        "点行内不该顺手把抽屉收了"
    );
}

/// 抽屉开着时,主条上的键也要真的点得到 —— 抽屉是加一层,不是罩一层。
#[test]
fn a_real_click_still_reaches_the_main_bar() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    key(&ui, "更多")
        .expect("找不到抽屉键")
        .invoke_accessible_default_action();

    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = asked.clone();
    ui.global::<Player>().on_next_track(move || {
        counter.set(counter.get() + 1);
    });

    let next = key(&ui, "下一首").expect("找不到下一首");
    click_at(&ui, &next);

    assert_eq!(
        asked.get(),
        1,
        "抽屉开着时下一首仍该点得动"
    );
}

/// 点条外收起。这一条盯的是修好层序之后那块收起区**还管用** ——
/// 把它压到底下容易,压到底下还接得住条外的点击才算数。
#[test]
fn a_real_click_outside_closes_the_drawer() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    key(&ui, "更多")
        .expect("找不到抽屉键")
        .invoke_accessible_default_action();
    assert!(present(&ui, "PlayerBar::drawer"));

    // 左上角:离条与抽屉都远。
    click_point(&ui, 8.0, 8.0);
    assert!(
        !present(&ui, "PlayerBar::drawer"),
        "点条外该收起抽屉"
    );
}

/// 开关的状态不靠图标明暗:开着与关着对外报的 checked 位不同,
/// 且每一行都有写明状态的文字标签。
/// 钉本次的起因 —— 深色档下 accent 与 accent-ink 曾是同一个色值。
#[test]
fn a_toggle_states_itself_in_words_not_only_in_color() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    key(&ui, "更多")
        .expect("找不到抽屉键")
        .invoke_accessible_default_action();

    ui.global::<Player>().set_shuffle_on(false);
    let off = key(&ui, "随机播放").expect("找不到随机");
    assert_eq!(off.accessible_checked(), Some(false));

    ui.global::<Player>().set_shuffle_on(true);
    let on = key(&ui, "随机播放").expect("找不到随机");
    assert_eq!(
        on.accessible_checked(),
        Some(true),
        "开着就该报开着 —— 读屏软件念的是这一位"
    );

    assert!(
        present(&ui, "DrawerRow::state-text"),
        "开关行该有写明状态的文字,不能只靠颜色"
    );
}

/// 循环三态各有各的文字:关 / 列表 / 单曲。
/// checked 只说得出开没开,说不出是哪一种。
#[test]
fn the_loop_row_names_all_three_states() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    key(&ui, "更多")
        .expect("找不到抽屉键")
        .invoke_accessible_default_action();

    ui.global::<Player>().set_loop_mode(0);
    assert!(key(&ui, "循环: 关").is_some());

    ui.global::<Player>().set_loop_mode(1);
    assert!(key(&ui, "循环: 列表").is_some());
    assert!(
        key(&ui, "循环: 关").is_none(),
        "换态之后旧标签不该还在"
    );

    ui.global::<Player>().set_loop_mode(2);
    assert!(key(&ui, "循环: 单曲").is_some());
}

/// 拨开关只喊一声,值由 Rust 写回 —— 界面不自置位。
#[test]
fn a_toggle_asks_without_setting_the_property() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    key(&ui, "更多")
        .expect("找不到抽屉键")
        .invoke_accessible_default_action();
    ui.global::<Player>().set_shuffle_on(false);

    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = asked.clone();
    ui.global::<Player>().on_shuffle_toggled(move || {
        counter.set(counter.get() + 1);
    });

    key(&ui, "随机播放")
        .expect("找不到随机")
        .invoke_accessible_default_action();

    assert_eq!(asked.get(), 1, "拨一下该喊一声");
    assert!(
        !ui.global::<Player>().get_shuffle_on(),
        "值该纹丝不动 —— 写它是 Rust 的活"
    );
}

/// 抽屉展开时主条仍在、仍可操作:抽屉是加一层,不是换一页。
#[test]
fn the_main_bar_stays_usable_while_the_drawer_is_open() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    key(&ui, "更多")
        .expect("找不到抽屉键")
        .invoke_accessible_default_action();

    assert!(present(&ui, "PlayerBar::capsule"), "主条该还在");
    assert!(key(&ui, "上一首").is_some());
    assert!(key(&ui, "下一首").is_some());

    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = asked.clone();
    ui.global::<Player>().on_toggle_play(move || {
        counter.set(counter.get() + 1);
    });
    key(&ui, "播放")
        .or_else(|| key(&ui, "暂停"))
        .expect("找不到播放键")
        .invoke_accessible_default_action();
    assert_eq!(asked.get(), 1, "抽屉开着时播放键仍该管用");
}

// ============ 尺寸回写(两处槽不合并)============

/// 条身把尺寸回写给 GPU 背景通道;播放页覆层那份走另一槽 ——
/// 两条可以同时在场,共用一槽会互相踩掉对方的尺寸。
#[test]
fn each_bar_mirrors_its_size_into_its_own_slot() {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    let _ = key(&ui, "上一首");

    assert!(
        ui.global::<Shell>().get_bar_w() > 0.0,
        "页内那条该把宽度回写,实得 {}",
        ui.global::<Shell>().get_bar_w()
    );
    assert!(ui.global::<Shell>().get_bar_h() > 0.0);

    ui.global::<Shell>().set_play_page_open(true);
    let _ = key(&ui, "收起播放页");
    assert!(
        ui.global::<Shell>().get_viz_bar_w() > 0.0,
        "覆层那条该把尺寸回写给自己那一槽"
    );
}
