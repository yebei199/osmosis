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

use crate::MainWindow;

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

    let rows = ui.get_tracks();
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

/// 接上红心键。
pub fn bind(ui: &MainWindow, set: &LikedSet) {
    let set = set.clone();
    let weak = ui.as_weak();

    ui.on_toggle_liked(move |track_id, liked| {
        let Some(ui) = weak.upgrade() else { return };

        // 先改本地、先变色。等服务端答复再变的话,手指底下没有反馈,人会连点。
        if !set_liked(&set, track_id.as_str(), liked) {
            // 集合没变说明界面与集合已经不一致了,重标一遍把它拉回来
            remark(&set, &ui);
            return;
        }
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

            let Some(ui) = weak.upgrade() else { return };

            // 失败要撤回:留着一个假的红心,下次进来就变回去了,
            // 而用户以为自己点成功了。
            set_liked(&set, &track_id, !liked);
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
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_of(ids: &[&str]) -> LikedSet {
        Rc::new(RefCell::new(
            ids.iter().map(|id| (*id).to_owned()).collect(),
        ))
    }

    /// 在集合里的算红心,不在的不算。
    #[test]
    fn rows_are_marked_against_the_liked_set() {
        let set = set_of(&["1", "3"]);

        assert!(is_liked(&set, "1"));
        assert!(is_liked(&set, "3"));
        assert!(!is_liked(&set, "2"));
    }

    /// 点下去立刻改本地集合 —— 等服务端来回一趟才变色的话,
    /// 手指底下没有任何反馈,人会连点。
    #[test]
    fn toggling_updates_the_set_immediately() {
        let set = set_of(&[]);

        assert!(
            set_liked(&set, "1", true),
            "本来没有,加上算改动"
        );
        assert!(is_liked(&set, "1"));

        assert!(
            set_liked(&set, "1", false),
            "本来有,去掉算改动"
        );
        assert!(!is_liked(&set, "1"));
    }

    /// 重复点同一个方向不算改动 —— 回滚要靠这个返回值,
    /// 没改动的话撤回也无从撤起。
    #[test]
    fn a_no_op_toggle_reports_no_change() {
        let set = set_of(&["1"]);

        assert!(!set_liked(&set, "1", true), "已经红心了");
        assert!(
            !set_liked(&set, "2", false),
            "本来就没红心"
        );
    }

    /// 撤回把集合还原成点之前的样子。
    ///
    /// 留着一个假的红心,下次进来就变回去了,而用户以为自己点成功了。
    #[test]
    fn a_failed_toggle_rolls_back() {
        let set = set_of(&["1"]);

        // 用户点了「取消红心」
        set_liked(&set, "1", false);
        assert!(!is_liked(&set, "1"));

        // 请求失败,撤回
        set_liked(&set, "1", true);
        assert!(
            is_liked(&set, "1"),
            "撤回之后该回到点之前的样子"
        );
    }
}
