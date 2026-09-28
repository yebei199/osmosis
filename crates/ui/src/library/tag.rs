//! 标签选择器的绑定(#158):拉全部标签、这首歌打了哪些,勾选/新建时发请求。
//!
//! 打的是哪一首由 `Library.track-menu-id` 认 —— 菜单里点「标签…」时藏起菜单
//! 但不清它(见 app.slint),选择器全程借这个值当目标。

use slint::{ComponentHandle, Model as _, VecModel};

use crate::Library;
use crate::MainWindow;
use crate::TagRow;

/// 写请求里的平台名。理由与 `crate::library::playlist::ONLY_PLATFORM` 同一条:
/// 接第二个平台之前,界面上每一行只有一个 id,没有平台可带。
const ONLY_PLATFORM: &str = "netease";

/// 拉这个账号的全部标签、当前这首歌打了哪些,合并填进 `all-tags`。
fn refresh(ui: &MainWindow, track_id: String) {
    let weak = ui.as_weak();
    let _ = slint::spawn_local(async move {
        let all = api::tags().await;
        let applied =
            api::track_tags(ONLY_PLATFORM, &track_id).await;
        let Some(ui) = weak.upgrade() else { return };

        let all = match all {
            Ok(dto) => dto.tags,
            Err(err) => {
                report(&ui, &err, "取标签失败");
                return;
            }
        };
        let applied: std::collections::HashSet<String> =
            applied
                .map(|dto| {
                    dto.tags
                        .into_iter()
                        .map(|tag| tag.id)
                        .collect()
                })
                .unwrap_or_default();

        let rows: Vec<TagRow> = all
            .into_iter()
            .map(|tag| TagRow {
                applied: applied.contains(&tag.id),
                id: tag.id.into(),
                name: tag.name.into(),
            })
            .collect();
        ui.global::<Library>().set_all_tags(
            slint::ModelRc::new(VecModel::from(rows)),
        );
    });
}

/// 就地改一行的勾选状态,不重拉整张表 —— 拉一遍要一趟往返,
/// 而勾选的反馈要跟得上手指(与红心同一条理由,见 `crate::library::liked`)。
fn mark(ui: &MainWindow, tag_id: &str, applied: bool) {
    let model = ui.global::<Library>().get_all_tags();
    for i in 0..model.row_count() {
        let Some(mut row) = model.row_data(i) else {
            continue;
        };
        if row.id == tag_id {
            row.applied = applied;
            model.set_row_data(i, row);
            return;
        }
    }
}

/// 当前菜单指向的曲目 id。空串 = 菜单没开,调用方据此跳过。
fn current_track(ui: &MainWindow) -> Option<String> {
    let id = ui.global::<Library>().get_track_menu_id();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// 接上标签选择器的三个回调:打开、勾选/取消、新建。
pub fn bind(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.global::<Library>().on_open_tag_picker(move || {
        let Some(ui) = weak.upgrade() else { return };
        let Some(track_id) = current_track(&ui) else {
            return;
        };
        refresh(&ui, track_id);
    });

    let weak = ui.as_weak();
    ui.global::<Library>().on_toggle_track_tag(
        move |tag_id, on| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(track_id) = current_track(&ui) else {
                return;
            };

            // 先勾上/去掉,再发请求;失败了撤回 —— 与红心同一条理由。
            mark(&ui, tag_id.as_str(), on);

            let weak = weak.clone();
            let tag_id = tag_id.to_string();
            let _ = slint::spawn_local(async move {
                let done = api::set_track_tag(
                    &tag_id,
                    ONLY_PLATFORM,
                    &track_id,
                    on,
                )
                .await;
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                if let Err(err) = done {
                    mark(&ui, &tag_id, !on);
                    report(&ui, &err, "标签没能保存");
                }
            });
        },
    );

    let weak = ui.as_weak();
    ui.global::<Library>().on_create_track_tag(
        move |name| {
            let name = name.trim().to_owned();
            let Some(ui) = weak.upgrade() else { return };
            if name.is_empty() {
                crate::notice::show(
                    &ui,
                    "标签要有名字".to_owned(),
                );
                return;
            }
            let Some(track_id) = current_track(&ui) else {
                return;
            };

            let weak = weak.clone();
            let _ = slint::spawn_local(async move {
                let created = api::create_tag(&name).await;
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                let tag = match created {
                    Ok(tag) => tag,
                    Err(err) => {
                        report(&ui, &err, "建标签失败");
                        return;
                    }
                };

                let done = api::set_track_tag(
                    &tag.id,
                    ONLY_PLATFORM,
                    &track_id,
                    true,
                )
                .await;
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                match done {
                    Ok(()) => refresh(&ui, track_id),
                    Err(err) => report(
                        &ui,
                        &err,
                        "标签没能保存",
                    ),
                }
            });
        },
    );
}

/// 报一次失败。走横幅,不走播放状态行(见 `crate::notice`)。
fn report(
    ui: &MainWindow,
    err: &api::ApiError,
    what: &str,
) {
    if crate::pages::account::handle_session_expiry(ui, err)
    {
        return;
    }
    crate::notice::show(ui, format!("{what}: {err}"));
}
