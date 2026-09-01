//! 歌单的写操作绑定:新建、改名、删除、批量加入与移出。

use slint::ComponentHandle;

use crate::Library;
use crate::MainWindow;

use super::*;

/// 接上本地歌单的写操作。
///
/// `reload` 由 `music` 传进来:改完之后要把当前歌单的曲目重取一遍,而那要用到
/// 播放队列,队列归那边。
pub fn bind_edit<R>(
    ui: &MainWindow,
    editing: &Editing,
    art: &crate::artwork::Artwork,
    reload: R,
) where
    R: Fn(&MainWindow) + Clone + 'static,
{
    bind_create(ui, art);
    bind_rename(ui, editing, art);
    bind_delete(ui, editing, art);
    bind_add_batch(ui, editing, reload.clone());
    bind_remove(ui, editing, reload);
}

/// 新建歌单。名字由回调带出来 —— 输入框在 `if` 里,Rust 引用不到它。
pub(super) fn bind_create(
    ui: &MainWindow,
    art: &crate::artwork::Artwork,
) {
    let art = art.clone();
    let weak = ui.as_weak();

    ui.global::<Library>().on_create_playlist(
        move |name| {
            let name = name.trim().to_owned();
            let Some(ui) = weak.upgrade() else { return };
            // 空名字服务端也会拒,但那要等一趟往返才说 ——
            // 而「没打字就按了新建」这件事这边就看得见。
            if name.is_empty() {
                crate::notice::show(
                    &ui,
                    "歌单要有名字".to_owned(),
                );
                return;
            }

            let art = art.clone();
            let weak = ui.as_weak();
            let _ = slint::spawn_local(async move {
                let done =
                    api::create_playlist(&name).await;
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                match done {
                    Ok(_) => refresh(&ui, &art),
                    Err(err) => {
                        report(&ui, &err, "建歌单失败")
                    }
                }
            });
        },
    );
}

/// 改名。
pub(super) fn bind_rename(
    ui: &MainWindow,
    editing: &Editing,
    art: &crate::artwork::Artwork,
) {
    let art = art.clone();
    let editing = editing.clone();
    let weak = ui.as_weak();

    ui.global::<Library>().on_rename_playlist(
        move |name| {
            let name = name.trim().to_owned();
            let Some(ui) = weak.upgrade() else { return };
            let Some(id) = editing.current_local() else {
                return;
            };
            if name.is_empty() {
                crate::notice::show(
                    &ui,
                    "歌单要有名字".to_owned(),
                );
                return;
            }

            let art = art.clone();
            let weak = ui.as_weak();
            let _ = slint::spawn_local(async move {
                let done =
                    api::rename_playlist(&id, &name).await;
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                match done {
                    Ok(()) => {
                        // 标题就地改掉,不等列表刷新 —— 详情页正显示着它
                        ui.global::<Library>()
                            .set_open_playlist_name(
                                name.as_str().into(),
                            );
                        refresh(&ui, &art);
                    }
                    Err(err) => {
                        report(&ui, &err, "改名失败")
                    }
                }
            });
        },
    );
}

/// 删除。二次确认由界面那一层管(见 app.slint),到这里已经是确定要删了。
pub(super) fn bind_delete(
    ui: &MainWindow,
    editing: &Editing,
    art: &crate::artwork::Artwork,
) {
    let art = art.clone();
    let editing = editing.clone();
    let weak = ui.as_weak();

    ui.global::<Library>().on_delete_playlist(move || {
        let Some(id) = editing.current_local() else {
            return;
        };
        let editing = editing.clone();
        let art = art.clone();
        let weak = weak.clone();

        let _ = slint::spawn_local(async move {
            let done = api::delete_playlist(&id).await;
            let Some(ui) = weak.upgrade() else { return };
            match done {
                Ok(()) => {
                    // 删掉的歌单不能再停在它的详情里
                    editing.closed();
                    ui.global::<Library>()
                        .set_open_playlist_name(
                            slint::SharedString::new(),
                        );
                    ui.global::<Library>()
                        .set_open_playlist_local(false);
                    ui.global::<Library>()
                        .set_add_batch_text(
                            slint::SharedString::new(),
                        );
                    refresh(&ui, &art);
                }
                Err(err) => report(&ui, &err, "删歌单失败"),
            }
        });
    });
}

/// 把打开之前那一批歌收进当前歌单。
pub(super) fn bind_add_batch<R>(
    ui: &MainWindow,
    editing: &Editing,
    reload: R,
) where
    R: Fn(&MainWindow) + Clone + 'static,
{
    let editing = editing.clone();
    let weak = ui.as_weak();

    ui.global::<Library>().on_add_batch(move || {
        let Some(id) = editing.current_local() else {
            return;
        };
        let refs = refs_of(&editing.stashed());
        if refs.is_empty() {
            return;
        }

        let weak = weak.clone();
        let editing = editing.clone();
        let reload = reload.clone();
        let _ = slint::spawn_local(async move {
            let done =
                api::add_playlist_tracks(&id, &refs).await;
            let Some(ui) = weak.upgrade() else { return };
            match done {
                Ok(()) => {
                    // 收完就没有「刚才那批」了:那一行的活干完了。
                    // 留着的话再点一次是把同一批又加一遍(服务端幂等,
                    // 但界面上看着像什么都没发生)。
                    editing.clear_stash();
                    ui.global::<Library>()
                        .set_add_batch_text(
                            slint::SharedString::new(),
                        );
                    reload(&ui);
                }
                Err(err) => report(&ui, &err, "加歌失败"),
            }
        });
    });
}

/// 把某一首移出当前歌单。
pub(super) fn bind_remove<R>(
    ui: &MainWindow,
    editing: &Editing,
    reload: R,
) where
    R: Fn(&MainWindow) + Clone + 'static,
{
    let editing = editing.clone();
    let weak = ui.as_weak();

    ui.global::<Library>().on_remove_track(
        move |track_id| {
            let Some(id) = editing.current_local() else {
                return;
            };
            let refs = vec![(
                ONLY_PLATFORM.to_owned(),
                track_id.to_string(),
            )];

            let weak = weak.clone();
            let reload = reload.clone();
            let _ = slint::spawn_local(async move {
                let done =
                    api::remove_playlist_tracks(&id, &refs)
                        .await;
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                match done {
                    Ok(()) => reload(&ui),
                    Err(err) => {
                        report(&ui, &err, "移出失败")
                    }
                }
            });
        },
    );
}
