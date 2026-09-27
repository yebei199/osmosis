//! 三态赞踩(#157):正在放的这一首,以及点一下之后发生什么。
//!
//! 独立于红心 —— 红心管收藏,这里管评价。只投影当前这一首,不像红心那样
//! 存一份全量集合:界面上没有别的地方要问"这首赞踩没有"。

use slint::ComponentHandle;

use crate::Library;
use crate::MainWindow;
use crate::Player;

/// 拉一次当前曲目的赞踩,填进 `Player.now-feedback`。
///
/// 失败不报错,只留 0(没表态)—— 与红心同一个理由:这是装饰,拉不到
/// 不该挡住听歌。换歌换得快时,答复回来时可能已经不是这一首了,那就
/// 不画 —— 不然会把上一首的答案错标到新歌头上。
pub fn refresh(ui: &MainWindow, track_id: &str) {
    if track_id.is_empty() {
        ui.global::<Player>().set_now_feedback(0);
        return;
    }

    let weak = ui.as_weak();
    let track_id = track_id.to_owned();
    let _ = slint::spawn_local(async move {
        let verdict = match api::feedback(&track_id).await {
            Ok(dto) => dto.verdict.unwrap_or(0),
            Err(err) => {
                log::warn!("取赞踩状态失败: {err}");
                0
            }
        };
        let Some(ui) = weak.upgrade() else { return };
        if ui.global::<Player>().get_now_id() == track_id {
            ui.global::<Player>()
                .set_now_feedback(i32::from(verdict));
        }
    });
}

/// 接上赞踩键。
pub fn bind(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.global::<Library>().on_toggle_feedback(
        move |verdict| {
            let Some(ui) = weak.upgrade() else { return };
            let track_id =
                ui.global::<Player>().get_now_id();
            if track_id.is_empty() {
                return;
            }

            let previous =
                ui.global::<Player>().get_now_feedback();
            // 再点已生效的那一档:取消。否则设成新值(覆盖或从无到有)。
            let next = if previous == verdict {
                0
            } else {
                verdict
            };

            // 乐观更新:先改界面,再发请求。等一趟往返才变色的话,
            // 手指底下没有反馈,人会连点(与红心同一个理由)。
            ui.global::<Player>().set_now_feedback(next);

            let weak = weak.clone();
            let track_id = track_id.to_string();
            let _ = slint::spawn_local(async move {
                #[allow(
                    clippy::cast_possible_truncation
                )]
                let result = if next == 0 {
                    api::clear_feedback(&track_id).await
                } else {
                    api::set_feedback(&track_id, next as i16)
                        .await
                };
                let Err(err) = result else { return };
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                // 失败要撤回,且只在还停在这一首时才动 —— 已经换歌的话,
                // 这条迟到的答复不该覆盖新歌的状态。
                if ui.global::<Player>().get_now_id()
                    == track_id
                {
                    ui.global::<Player>()
                        .set_now_feedback(previous);
                }
                crate::notice::show(
                    &ui,
                    format!("赞踩没能保存: {err}"),
                );
            });
        },
    );
}

#[cfg(test)]
mod tests;
