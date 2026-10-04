//! 顶部状态条给页面让位，覆盖两种版式、登录页与播放页。

use i_slint_backend_testing as testing;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, ModelRc, VecModel};
use ui::{
    Library, MainWindow, PlaylistRow, Session, Shell,
};

fn window(width: f32) -> MainWindow {
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.window()
        .set_size(slint::LogicalSize::new(width, 800.0));
    ui.global::<Shell>().set_compact(width < 600.0);
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_current_tab(1);
    ui.global::<Shell>().set_music_section(1);
    ui
}

fn element(
    ui: &MainWindow,
    id: &str,
) -> testing::ElementHandle {
    testing::mock_elapsed_time(std::time::Duration::ZERO);
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
        .unwrap_or_else(|| panic!("找不到 {id}"))
}

fn bottom(h: &testing::ElementHandle) -> f32 {
    h.absolute_position().y + h.size().height
}

fn strips(ui: &MainWindow, mask: u8) {
    let shell = ui.global::<Shell>();
    shell.set_banner_text(
        if mask & 1 != 0 { "网络断开" } else { "" }.into(),
    );
    shell.set_group_banner(
        if mask & 2 != 0 {
            "与测试设备一起播放"
        } else {
            ""
        }
        .into(),
    );
    shell.set_download_text(
        if mask & 4 != 0 { "下载中 42%" } else { "" }
            .into(),
    );
    // 组提示由状态观察器展开；清除时让200ms收起动画落地再量布局。
    testing::mock_elapsed_time(std::time::Duration::ZERO);
    if mask & 2 == 0 {
        testing::mock_elapsed_time(
            std::time::Duration::from_millis(200),
        );
    }
}

fn click(
    ui: &MainWindow,
    position: slint::LogicalPosition,
) {
    ui.window().dispatch_event(WindowEvent::PointerMoved {
        position,
    });
    ui.window().dispatch_event(
        WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        },
    );
    ui.window().dispatch_event(
        WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        },
    );
}

/// F-002：在真实输入框填字，开关播放页后重新查框，防止旧句柄掩盖重建。
#[test]
fn search_text_survives_opening_and_closing_the_play_page()
{
    testing::init_no_event_loop();
    for width in [420.0, 1000.0] {
        for mask in [0, 7] {
            let ui = window(width);
            strips(&ui, mask);
            ui.global::<Shell>().set_music_section(2);
            let keyword =
                element(&ui, "MusicPage::keyword");
            keyword.set_accessible_value("retained query");
            similar_asserts::assert_eq!(
                keyword.accessible_value().as_deref(),
                Some("retained query")
            );
            ui.global::<Shell>().set_play_page_open(true);
            // 查询过滤隐藏元素；弱句柄存活才说明音乐壳没有销毁。
            let shell_retained = keyword.is_valid();
            ui.global::<Shell>().set_play_page_open(false);
            similar_asserts::assert_eq!(
                element(&ui, "MusicPage::keyword")
                    .accessible_value()
                    .as_deref(),
                Some("retained query"),
                "搜索文字丢失，width={width} mask={mask}"
            );
            assert!(
                shell_retained,
                "播放页打开时音乐壳不该销毁"
            );
        }
    }
}

/// 隐藏壳的首行不能接收指针，关闭播放页后同一位置仍可进入详情。
#[test]
fn the_play_page_blocks_pointer_input_to_the_retained_shell()
 {
    testing::init_no_event_loop();
    for width in [420.0, 1000.0] {
        for mask in [0, 7] {
            let ui = window(width);
            strips(&ui, mask);
            ui.global::<Library>().set_playlists(
                ModelRc::new(VecModel::from(vec![
                    PlaylistRow {
                        id: "first".into(),
                        name: "测试歌单".into(),
                        subtitle: "1 首".into(),
                        source: 2,
                        cover: Default::default(),
                    },
                ])),
            );
            let weak = ui.as_weak();
            ui.global::<Library>().on_open_playlist(
                move |id, _| {
                    similar_asserts::assert_eq!(
                        id, "first"
                    );
                    weak.upgrade()
                        .expect("点击时窗口仍存在")
                        .global::<Library>()
                        .set_open_playlist_name(
                            "测试歌单".into(),
                        );
                },
            );
            let row = element(&ui, "PlaylistList::touch");
            let position = slint::LogicalPosition::new(
                row.absolute_position().x
                    + row.size().width / 2.0,
                row.absolute_position().y
                    + row.size().height / 2.0,
            );
            ui.global::<Shell>().set_play_page_open(true);
            click(&ui, position);
            assert!(
                ui.global::<Library>()
                    .get_open_playlist_name()
                    .is_empty(),
                "播放页下的歌单收到了点击，width={width} mask={mask}"
            );
            ui.global::<Shell>().set_play_page_open(false);
            click(&ui, position);
            assert!(
                testing::ElementHandle::find_by_element_id(
                    &ui,
                    "MusicPage::playlist-header",
                )
                .next()
                .is_some(),
                "关闭播放页后歌单仍该可点"
            );
        }
    }
}

/// 每一种状态组合都实际量边界；收起条后页面回到原位。
#[test]
fn every_strip_combination_reserves_space_in_both_layouts()
{
    testing::init_no_event_loop();
    for width in [420.0, 1000.0] {
        let ui = window(width);
        let page_id = if width < 600.0 {
            "MusicPage::music-bar"
        } else {
            "MusicPage::playlist-list"
        };
        let initial_y =
            element(&ui, page_id).absolute_position().y;
        for mask in 1..8 {
            strips(&ui, mask);
            let mut boxes: Vec<_> = [
                "MainWindow::banner",
                "MainWindow::group-strip",
                "MainWindow::download-strip",
            ]
            .into_iter()
            .filter_map(|id| {
                testing::ElementHandle::find_by_element_id(
                    &ui, id,
                )
                .next()
            })
            .collect();
            boxes.sort_by(|a, b| {
                a.absolute_position()
                    .y
                    .total_cmp(&b.absolute_position().y)
            });
            for pair in boxes.windows(2) {
                assert!(
                    bottom(&pair[0])
                        <= pair[1].absolute_position().y,
                    "状态条互相遮挡，mask={mask}"
                );
            }
            let last =
                boxes.last().expect("至少有一条状态条");
            assert!(
                element(&ui, page_id).absolute_position().y
                    >= bottom(last),
                "页面被状态条遮挡，width={width} mask={mask}"
            );
            assert!(
                element(&ui, page_id).absolute_position().y
                    > initial_y,
                "状态条未给页面让位"
            );
        }
        strips(&ui, 0);
        assert_eq!(
            element(&ui, page_id).absolute_position().y,
            initial_y
        );
    }
}

/// 背景铺满窗口时，登录表单和播放内容仍共用状态条的让位空间。
#[test]
fn login_and_play_pages_move_below_the_status_stack() {
    testing::init_no_event_loop();
    let ui = window(420.0);
    ui.global::<Session>().set_logged_in(false);
    strips(&ui, 7);
    let end =
        bottom(&element(&ui, "MainWindow::download-strip"));
    assert!(
        element(&ui, "LoginPage::username")
            .absolute_position()
            .y
            >= end
    );
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Shell>().set_play_page_open(true);
    assert!(
        element(&ui, "PlayPage::play-title")
            .absolute_position()
            .y
            >= end
    );
}

/// 两种组状态的操作都达到触控下限，保留原来的加入与退出标签。
#[test]
fn group_actions_have_a_full_touch_target() {
    testing::init_no_event_loop();
    let ui = window(420.0);
    strips(&ui, 2);
    for (joinable, label) in
        [(true, "加入播放组"), (false, "退出")]
    {
        ui.global::<Shell>().set_group_joinable(joinable);
        testing::mock_elapsed_time(
            std::time::Duration::ZERO,
        );
        let action = testing::ElementHandle::find_by_accessible_label(&ui, label).next().expect("组操作该存在");
        assert!(
            action.size().height >= 44.0,
            "{label} 高度不足44px"
        );
    }
}

/// 歌单首行走指针命中测试；回调把详情页打开，防止无障碍动作绕过遮挡。
#[test]
fn the_first_playlist_opens_through_a_real_pointer_click() {
    testing::init_no_event_loop();
    for width in [420.0, 1000.0] {
        let ui = window(width);
        ui.global::<Library>().set_playlists(ModelRc::new(
            VecModel::from(vec![PlaylistRow {
                id: "first".into(),
                name: "测试歌单".into(),
                subtitle: "1 首".into(),
                source: 2,
                cover: Default::default(),
            }]),
        ));
        let weak = ui.as_weak();
        ui.global::<Library>().on_open_playlist(
            move |id, _| {
                let ui = weak
                    .upgrade()
                    .expect("点击时窗口仍存在");
                assert_eq!(id, "first");
                ui.global::<Library>()
                    .set_open_playlist_name(
                        "测试歌单".into(),
                    );
            },
        );
        strips(&ui, 7);
        let row = element(&ui, "PlaylistList::touch");
        assert!(
            row.absolute_position().y
                >= bottom(&element(
                    &ui,
                    "MainWindow::download-strip"
                ))
        );
        let position = slint::LogicalPosition::new(
            row.absolute_position().x
                + row.size().width / 2.0,
            row.absolute_position().y
                + row.size().height / 2.0,
        );
        ui.window().dispatch_event(
            WindowEvent::PointerMoved { position },
        );
        ui.window().dispatch_event(
            WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            },
        );
        ui.window().dispatch_event(
            WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            },
        );
        assert!(
            testing::ElementHandle::find_by_element_id(
                &ui,
                "MusicPage::playlist-header"
            )
            .next()
            .is_some(),
            "首行点击未打开歌单详情"
        );
    }
}

/// F-001：有条、无条都铺满背景，标题与控制条只在可用内容区内。
#[test]
fn play_background_fills_the_window_without_covering_content()
 {
    testing::init_no_event_loop();
    for width in [420.0, 1000.0] {
        let ui = window(width);
        ui.global::<Shell>().set_play_page_open(true);
        for mask in 0..8 {
            strips(&ui, mask);
            let page = testing::ElementHandle::find_by_element_type_name(&ui, "PlayPage")
                .next().expect("播放页该展开");
            let background = page
                .query_descendants()
                .match_type_name("Rectangle")
                .find_first()
                .expect("播放页该有背景");
            assert_eq!(
                background.absolute_position().y,
                0.0,
                "背景被状态条截断，width={width} mask={mask}"
            );
            assert_eq!(
                background.size().height,
                800.0,
                "播放背景未铺满窗口，width={width} mask={mask}"
            );
            let end = [
                "MainWindow::banner",
                "MainWindow::group-strip",
                "MainWindow::download-strip",
            ]
            .into_iter()
            .filter_map(|id| {
                testing::ElementHandle::find_by_element_id(
                    &ui, id,
                )
                .next()
            })
            .map(|h| bottom(&h))
            .fold(0.0_f32, f32::max);
            for id in [
                "PlayPage::play-title",
                "PlayPage::player-bar",
            ] {
                assert!(
                    element(&ui, id).absolute_position().y
                        >= end,
                    "播放内容被状态条覆盖，{id} width={width} mask={mask}"
                );
            }
        }
    }
}

/// 登录背景同样铺到屏顶，表单不能退回状态条下面的不可点击区域。
#[test]
fn login_background_fills_the_window_while_the_form_avoids_strips()
 {
    testing::init_no_event_loop();
    for width in [420.0, 1000.0] {
        let ui = window(width);
        ui.global::<Session>().set_logged_in(false);
        for mask in 0..8 {
            strips(&ui, mask);
            let background = testing::ElementHandle::find_by_element_type_name(&ui, "AuroraBackground")
                .next().expect("登录页该有极光背景");
            assert_eq!(
                background.absolute_position().y,
                0.0,
                "登录背景被状态条截断，width={width} mask={mask}"
            );
            assert_eq!(
                background.size().height,
                800.0,
                "登录背景未铺满窗口，width={width} mask={mask}"
            );
            let end = [
                "MainWindow::banner",
                "MainWindow::group-strip",
                "MainWindow::download-strip",
            ]
            .into_iter()
            .filter_map(|id| {
                testing::ElementHandle::find_by_element_id(
                    &ui, id,
                )
                .next()
            })
            .map(|h| bottom(&h))
            .fold(0.0_f32, f32::max);
            assert!(
                element(&ui, "LoginPage::username")
                    .absolute_position()
                    .y
                    >= end,
                "登录表单被状态条覆盖，width={width} mask={mask}"
            );
        }
    }
}
