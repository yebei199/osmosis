//! 屏蔽规则的客户端侧(#161):手上一份规则,队列前进时拿它跳过命中的歌。
//!
//! 列表的隐藏在服务端出口做,这里不重复;这份规则只管**已经装进队列**的
//! 那一批 —— 它们是规则建立之前拿到的。

use std::cell::RefCell;
use std::rc::Rc;

use app_core::{BlockRuleDto, TrackDto};

/// 这个账号的屏蔽规则。启动时拉一次,本机建、删之后就地改。
pub type BlockSet = Rc<RefCell<Vec<BlockRuleDto>>>;

/// 这首歌命不命中手上的规则。
pub fn hits(set: &BlockSet, track: &TrackDto) -> bool {
    app_core::blocks::hits(&set.borrow(), track)
}

/// 拉一次全部规则。失败只写日志:拿不到规则只是队列不跳过,不该挡住听歌。
pub fn refresh(set: &BlockSet) {
    let set = set.clone();
    let _ = slint::spawn_local(async move {
        match api::blocks().await {
            Ok(dto) => *set.borrow_mut() = dto.rules,
            Err(err) => log::warn!("取屏蔽规则失败: {err}"),
        }
    });
}
