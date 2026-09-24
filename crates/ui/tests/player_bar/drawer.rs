//! 抽屉那一层:向上长、到处一样、点得到、开关说得出自己的状态。

use super::*;

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

/// 随机、循环、音量都住在抽屉里,收起时不占主条。
#[test]
fn the_drawer_holds_the_modes() {
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
        !present(&ui, "SyncStrip::sync-empty"),
        "同播已删(#137),抽屉里不该还有同播那一行"
    );
}

/// 名册里的设备只以输出设备的身份出现(#137)。
///
/// 同播那一行删掉之后,同一台设备在抽屉里只剩一颗「输出到 xx」:点它是遥控
/// 那台去放。两行并存时点错了不报错,只是声音从另一台机器出来。
#[test]
fn listed_devices_show_up_only_as_outputs() {
    use slint::{ModelRc, VecModel};

    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_devices(ModelRc::new(
        VecModel::from(vec![ui::DeviceRow {
            id: "pc1".into(),
            name: "pc1".into(),
            member: false,
        }]),
    ));

    key(&ui, "更多")
        .expect("找不到抽屉键")
        .invoke_accessible_default_action();

    assert!(
        key(&ui, "输出到 pc1").is_some(),
        "名册里的设备该列成输出设备"
    );
    // 每台设备旁那颗小键把它加进 / 移出播放组(#137 ⑤),本机也有一颗。
    assert!(key(&ui, "加入 pc1").is_some(), "不在组里的显示「加入」");
    assert!(key(&ui, "移出 本机").is_some(), "本机默认就是那一台输出");
    assert!(
        !present(&ui, "SyncStrip::sync-label"),
        "同播那一行不该还在"
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
        check_drawer_grows_upward(
            &ui,
            &format!("tab {tab}"),
        );
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
    assert_eq!(after, before, "{page}:开抽屉不该动主条");

    let drawer = ids(ui, "PlayerBar::drawer")
        .into_iter()
        .next()
        .expect("抽屉该开着");
    let bottom =
        drawer.absolute_position().y + drawer.size().height;
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

    assert_eq!(inside, outside, "播放页的抽屉与别处不一致");
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

    assert!(
        present(&ui, "PlayerBar::capsule"),
        "主条该还在"
    );
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

// ============ 喜欢(#115)============

/// 开着抽屉、正在放 `id` 这一首。
fn drawer_playing(id: &str) -> MainWindow {
    let ui = playing_app();
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Player>().set_now_id(id.into());
    key(&ui, "更多")
        .expect("找不到抽屉键")
        .invoke_accessible_default_action();
    ui
}

/// 抽屉那一行照着 `now-liked` 报自己亮没亮,读屏念得出来。
#[test]
fn the_like_row_reports_the_projected_state() {
    let ui = drawer_playing("1");

    ui.global::<Player>().set_now_liked(false);
    let off =
        key(&ui, "喜欢这一首").expect("抽屉里该有喜欢");
    assert_eq!(off.accessible_checked(), Some(false));

    ui.global::<Player>().set_now_liked(true);
    let on =
        key(&ui, "喜欢这一首").expect("抽屉里该有喜欢");
    assert_eq!(
        on.accessible_checked(),
        Some(true),
        "已喜欢的歌,那一行该报开着"
    );
}

/// 真按一下:喊 toggle-liked,参数是正在放的那首与点完之后的状态。
/// 值由 Rust 写回,界面不自置位。
#[test]
fn a_real_click_on_like_asks_to_flip_the_playing_track() {
    let ui = drawer_playing("42");
    ui.global::<Player>().set_now_liked(false);

    let asked = std::rc::Rc::new(std::cell::RefCell::new(
        Vec::new(),
    ));
    let log = asked.clone();
    ui.global::<ui::Library>().on_toggle_liked(
        move |id, liked| {
            log.borrow_mut().push((id.to_string(), liked));
        },
    );

    let row =
        key(&ui, "喜欢这一首").expect("抽屉里该有喜欢");
    click_at(&ui, &row);

    assert_eq!(
        *asked.borrow(),
        vec![("42".to_owned(), true)]
    );
    assert!(
        !ui.global::<Player>().get_now_liked(),
        "值该纹丝不动 —— 写它是 Rust 的活"
    );
}

/// 没在放歌时点它什么也不发生 —— 没有「这一首」可喜欢。
#[test]
fn like_does_nothing_without_a_track() {
    let ui = drawer_playing("");

    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = asked.clone();
    ui.global::<ui::Library>().on_toggle_liked(
        move |_, _| {
            counter.set(counter.get() + 1);
        },
    );

    key(&ui, "喜欢这一首")
        .expect("行还在,只是灰着")
        .invoke_accessible_default_action();

    assert_eq!(asked.get(), 0, "手上没歌时不该喊");
}
