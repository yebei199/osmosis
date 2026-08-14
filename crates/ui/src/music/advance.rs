//! 预取下一首,以及队列的推进 —— 手动的与放完自动的两条路。

use super::*;
use crate::Player;

/// 取走备好的那一份 —— **只在它确实是这一首时**。
///
/// 不是这一首就地丢掉:用户中途点了列表里别的歌、或洗了牌,备的那一份再也用不上,
/// 留着只是占一个临时文件和一条还在跑的下载。认错了则更糟 —— 会放出一首
/// 根本没点过的歌。
///
/// 泛型是为了能单独测这道校验:备好的那一份是解码器,测试里造不出来,
/// 而这里唯一的逻辑是"id 对不对得上",与备的是什么东西无关。
pub(super) fn take_prefetched<T>(
    slot: &RefCell<Option<(String, T)>>,
    wanted: &str,
) -> Option<T> {
    let (id, ready) = slot.borrow_mut().take()?;
    (id == wanted).then_some(ready)
}

/// 备下一首:与正常播放走**同一个** [`prepare`],只是备好了先搁着。
///
/// 失败不声张:预取只是提速,它没成的话照常走原路,用户什么都不会察觉。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn start_prefetch(deck: &Deck) {
    let Some(track) =
        deck.queue.borrow().peek_next().cloned()
    else {
        return;
    };

    deck.prefetching.set(true);
    let deck = deck.clone();
    slint::spawn_local(async move {
        let ready =
            prepare(deck.player.clone(), track.clone())
                .await;
        deck.prefetching.set(false);
        match ready {
            Ok((decoded, health)) => {
                log::debug!("预取就绪: {}", track.title);
                *deck.prefetched.borrow_mut() =
                    Some((track.id, (decoded, health)));
            }
            Err(error) => {
                log::debug!("预取没成,照常走: {error}");
            }
        }
    })
    .expect("event loop must be running");
}

/// 手动「下一首」:队列前进一首;到底了就停下并说明
/// (循环语义见 `CONTEXT.md`「队列」)。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn advance(ui: &MainWindow, deck: &Deck) {
    let has_next = deck
        .queue
        .borrow_mut()
        .next(shuffle_seed())
        .is_some();
    after_advance(ui, deck, has_next);
}

/// 播完一首的自动推进:与手动只差队列入口 —— 单曲循环时留在本曲重放。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn advance_auto(ui: &MainWindow, deck: &Deck) {
    let has_next = deck
        .queue
        .borrow_mut()
        .advance_auto(shuffle_seed())
        .is_some();
    after_advance(ui, deck, has_next);
}

/// 推进之后的收尾:有歌就放,没有就停。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn after_advance(
    ui: &MainWindow,
    deck: &Deck,
    has_next: bool,
) {
    if has_next {
        play_current(ui, deck);
    } else {
        // 状态机也要停:不停的话它仍是 Playing,自动续播每秒都会再撞进来。
        deck.playback.borrow_mut().stop();
        ui.global::<Player>()
            .set_playback_text(QUEUE_DONE.into());
        ui.global::<Player>().set_is_playing(false);
    }
}
