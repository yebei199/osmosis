//! 红心:哪些歌在红心里,以及点一下之后发生什么。
//!
//! 服务端给的曲目**不带**「这首红心没有」——让每个列表接口都去问一次上游太贵。
//! 客户端取一次红心的全量标识存成集合,列表来了本地比对(见 `/liked/ids`)。
//!
//! 点红心是**乐观更新**:先改本地、先变色,再发请求;失败了撤回。等一趟往返
//! (网易云那边几百毫秒)才变色的话,手指底下没有任何反馈,人会连点。

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use slint::ComponentHandle;

use crate::Library;
use crate::MainWindow;
use crate::Player;

/// 红心里的曲目标识。
///
/// 目前只存 id 不存平台:服务端那侧也一样(见 `contract::TrackIdsDto`)。
/// 接第二个平台时两边一起改成 `(平台, id)`。
pub type LikedSet = Rc<RefCell<HashSet<String>>>;

/// 某一首在不在红心里。
pub fn is_liked(set: &LikedSet, track_id: &str) -> bool {
    set.borrow().contains(track_id)
}

/// 就地改一首的红心状态,返回是否真的改了。
///
/// 返回值给回滚用:没改动的话撤回也无从撤起。
pub fn set_liked(
    set: &LikedSet,
    track_id: &str,
    liked: bool,
) -> bool {
    let mut set = set.borrow_mut();
    if liked {
        set.insert(track_id.to_owned())
    } else {
        set.remove(track_id)
    }
}

/// 拉一次红心的全量标识。
///
/// 失败不报错,只是集合为空 —— 那意味着所有心画成空的。比整页失败好:
/// 红心状态是**装饰**,拉不到它不该挡住听歌。
pub fn refresh(set: &LikedSet, ui: &MainWindow) {
    let set = set.clone();
    let weak = ui.as_weak();

    let _ = slint::spawn_local(async move {
        match api::liked_ids().await {
            Ok(dto) => {
                *set.borrow_mut() =
                    dto.track_ids.into_iter().collect();
                if let Some(ui) = weak.upgrade() {
                    remark(&set, &ui);
                }
            }
            Err(err) => {
                if let Some(ui) = weak.upgrade() {
                    crate::account::handle_session_expiry(
                        &ui, &err,
                    );
                }
                log::warn!("取红心列表失败: {err}");
            }
        }
    });
}

/// 按当前集合重新标一遍列表里的每一行。
///
/// 列表换了一批歌、或红心集合变了,都要走这里 —— 两处各标各的话,
/// 换歌单之后心的状态会停在上一批。
pub fn remark(set: &LikedSet, ui: &MainWindow) {
    use slint::Model as _;

    let rows = ui.global::<Player>().get_tracks();
    for i in 0..rows.row_count() {
        let Some(mut row) = rows.row_data(i) else {
            continue;
        };
        let liked = is_liked(set, row.id.as_str());
        if row.liked != liked {
            row.liked = liked;
            rows.set_row_data(i, row);
        }
    }
}

/// 就地把「我喜欢的」那一行的条数加减一个数。
///
/// 那个数字来自 `/playlists`,而点红心不重拉它 —— 不动的话现象是心变红了、
/// 旁边的数字还是旧的,要切出歌单分区再回来才变。
///
/// 数字只存在于那一行的副标题里(界面上没有别处存着它),读不回来就**不动**:
/// 猜一个写上去比留着旧的更糟 —— 旧的下次拉 `/playlists` 会自愈,
/// 猜的那个会一直看起来很正常。
#[cfg(not(target_arch = "wasm32"))]
fn bump_liked_count(ui: &MainWindow, delta: i32) {
    use slint::Model;

    let rows = ui.global::<Library>().get_playlists();
    let liked = crate::playlist::Source::Liked.to_index();

    for index in 0..rows.row_count() {
        let Some(mut row) = rows.row_data(index) else {
            continue;
        };
        if row.source != liked {
            continue;
        }

        // 读不出条数就整个不动它,而不是从 0 起算
        let Some(count) =
            crate::playlist::track_count_of(&row.subtitle)
        else {
            return;
        };

        row.subtitle = crate::playlist::track_count_text(
            (count + delta).max(0),
        )
        .into();
        rows.set_row_data(index, row);
        return;
    }
}

/// wasm 上没有那一行可动。`playlist` 整个模块在门外(见 lib.rs),歌单列表
/// 因此从没被填过 —— 上面那个循环在 web 上本来也一条都匹配不到,空壳与它等价。
///
/// 留空壳而不是把调用点也用 cfg 圈起来:红心那条路各端共用一份代码,
/// 分叉一次就得在每个改动里维护两份。
#[cfg(target_arch = "wasm32")]
fn bump_liked_count(_ui: &MainWindow, _delta: i32) {}

/// 接上红心键。
pub fn bind(ui: &MainWindow, set: &LikedSet) {
    // 重拉。绑定阶段那一次跑在登录之前,拿不到 token —— 登录收尾与进列表页
    // 各喊一次这个回调,把集合补上(见 app.slint 的 refresh-liked)。
    let reloading = set.clone();
    let weak = ui.as_weak();
    ui.global::<Library>().on_refresh_liked(move || {
        let Some(ui) = weak.upgrade() else { return };
        refresh(&reloading, &ui);
    });

    let set = set.clone();
    let weak = ui.as_weak();

    ui.global::<Library>().on_toggle_liked(
        move |track_id, liked| {
            let Some(ui) = weak.upgrade() else { return };

            // 先改本地、先变色。等服务端答复再变的话,手指底下没有反馈,人会连点。
            if !set_liked(&set, track_id.as_str(), liked) {
                // 集合没变说明界面与集合已经不一致了,重标一遍把它拉回来
                remark(&set, &ui);
                return;
            }
            bump_liked_count(
                &ui,
                if liked { 1 } else { -1 },
            );
            remark(&set, &ui);

            let set = set.clone();
            let weak = weak.clone();
            let track_id = track_id.to_string();

            let _ = slint::spawn_local(async move {
                let Err(err) =
                    api::set_liked(&track_id, liked).await
                else {
                    return;
                };

                let Some(ui) = weak.upgrade() else {
                    return;
                };

                // 失败要撤回:留着一个假的红心,下次进来就变回去了,
                // 而用户以为自己点成功了。
                set_liked(&set, &track_id, !liked);
                bump_liked_count(
                    &ui,
                    if liked { -1 } else { 1 },
                );
                remark(&set, &ui);

                // 会话失效已经把人送回登录页了,不必再在音乐页写一句
                if !crate::account::handle_session_expiry(
                    &ui, &err,
                ) {
                    crate::notice::show(
                        &ui,
                        format!("红心没能保存: {err}"),
                    );
                }
            });
        },
    );
}

#[cfg(test)]
mod tests;
