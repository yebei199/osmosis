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

/// 页面覆层共用同一份让位空间，登录页同时缩短高度。
#[test]
fn login_and_play_pages_move_below_the_status_stack() {
    testing::init_no_event_loop();
    let ui = window(420.0);
    ui.global::<Session>().set_logged_in(false);
    let original = element(&ui, "MainWindow::login-page")
        .size()
        .height;
    strips(&ui, 7);
    let end =
        bottom(&element(&ui, "MainWindow::download-strip"));
    let login = element(&ui, "MainWindow::login-page");
    assert!(login.absolute_position().y >= end);
    assert!(login.size().height < original);
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
