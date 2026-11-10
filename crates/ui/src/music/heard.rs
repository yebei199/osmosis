//! 日推与电台起播的那一批里,已经「听过」的歌:自动续播跳过它们(用户 2026-11-10)。
//!
//! 听过是账号的事,不是这台设备的:同一批在手机上放着,其中一首在电脑上听完了,
//! 手机放到它时不该再放一遍。所以每起播一首、每收到一次「电台变了」,就把这一批
//! 还没放到的那些拿去问服务端(`POST /heard`),答回来的记下。只在自动续播时跳:
//! 用户点名的那一首照放,手动「下一首」也照常往下走。
//!
//! 只认日推与电台起播的批次 —— 歌单、搜索里的歌听没听过,与放不放它无关。

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use super::*;

/// 记下的听过的曲目,以及哪一批是从日推点出来的。
#[derive(Clone, Default)]
pub(super) struct Heard {
    ids: Rc<RefCell<HashSet<String>>>,
    daily_batch: Rc<Cell<Option<u64>>>,
}

impl Heard {
    /// 队列第 `batch` 批是在日推里点出来的。
    pub(super) fn mark_daily(&self, batch: u64) {
        self.daily_batch.set(Some(batch));
    }

    /// 服务端说这几首听过了。
    pub(super) fn learn(
        &self,
        ids: impl IntoIterator<Item = String>,
    ) {
        self.ids.borrow_mut().extend(ids);
    }
}

/// 队列此刻这一批是日推或电台起播的。
pub(super) fn sourced(deck: &Deck) -> bool {
    deck.heard.daily_batch.get()
        == Some(deck.queue.borrow().batch())
        || radio::owns_batch(deck)
}

/// 自动续播该跳过哪些:这一批是日推或电台的,其中已经听过的。
///
/// 判这一批是不是日推、电台的要读队列,所以在推进**之前**判好 —— 推进时队列正被
/// 可变借着。
pub(super) fn skipper(
    deck: &Deck,
) -> impl Fn(&TrackDto) -> bool + use<> {
    let ids = sourced(deck).then(|| deck.heard.ids.clone());
    move |track| {
        ids.as_ref().is_some_and(|ids| {
            ids.borrow().contains(&track.id)
        })
    }
}

/// 把这一批里还没放到的那些拿去问服务端,答回来的记下。不是日推或电台的批次不问;
/// 问不成只记日志 —— 下一首起播或下一条信令时还会再问。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn refresh(deck: &Deck) {
    if deck.group.is_member() || !sourced(deck) {
        return;
    }
    let ahead: Vec<String> = {
        let queue = deck.queue.borrow();
        let at = queue
            .order()
            .iter()
            .position(|index| *index == queue.index())
            .map_or(0, |at| at + 1);
        queue.order()[at..]
            .iter()
            .map(|index| queue.tracks()[*index].id.clone())
            .collect()
    };
    if ahead.is_empty() {
        return;
    }
    let heard = deck.heard.clone();
    let _ = slint::spawn_local(async move {
        match api::heard_among(ahead).await {
            Ok(ids) => heard.learn(ids),
            Err(error) => {
                log::debug!("问听过没有没问成: {error}");
            }
        }
    });
}
