//! 独奏重启之后接着上次:队列、放到哪一首、哪一秒都回来,停在暂停上,不自己放
//! (用户 2026-11-10,`docs/adr/0031` 的「队列活得比客户端久」落到独奏上)。
//!
//! 真相在服务端:队列是这台设备名下那一个(`GET /devices/{id}/queue`),放到哪是
//! 最近一次检查点([`super::checkpoint`] 报的条目与位置)。本机不另存一份。

use std::time::Duration;

use api::{QueueEntryDto, QueueHeadDto};

use super::*;
use crate::music::*;

/// 从服务端那份概况与整份条目里,认出该恢复成什么:原序的曲目、第几首、播放次序
/// (随机时)、从哪一毫秒起。没有检查点或条目对不上就是 `None` —— 不瞎猜一首放。
#[derive(Debug, Clone, PartialEq)]
pub(in crate::music) struct Resume {
    pub(in crate::music) tracks: Vec<TrackDto>,
    pub(in crate::music) entry_ids: Vec<i64>,
    pub(in crate::music) index: usize,
    /// 随机时的播放次序(曲目下标);不随机是 `None`。
    pub(in crate::music) order: Option<Vec<usize>>,
    pub(in crate::music) position: Duration,
    /// 停下那一段的起播事件与累计听了多久:接着往那一行上累计。
    pub(in crate::music) play: Option<api::PlayProgressDto>,
}

pub(in crate::music) fn resume_from(
    head: &QueueHeadDto,
    entries: Vec<QueueEntryDto>,
) -> Option<Resume> {
    let report = head.report.as_ref()?;
    let entry_ids: Vec<i64> = entries
        .iter()
        .map(|entry| entry.entry_id)
        .collect();
    let index = entry_ids
        .iter()
        .position(|id| Some(*id) == report.entry_id)?;
    let order =
        report.play_order.as_ref().and_then(|order| {
            let order: Option<Vec<usize>> = order
                .iter()
                .map(|id| {
                    entry_ids.iter().position(|e| e == id)
                })
                .collect();
            order.filter(|order| {
                order
                    .iter()
                    .enumerate()
                    .any(|(at, i)| at != *i)
            })
        });
    Some(Resume {
        tracks: entries
            .into_iter()
            .map(|entry| entry.track)
            .collect(),
        entry_ids,
        index,
        order,
        position: Duration::from_millis(
            u64::try_from(report.position_ms).unwrap_or(0),
        ),
        play: report.play,
    })
}

/// 进程里第一次进音乐页时找回上次的队列。手上已经有歌、或在组里,就不动。
/// 取不到(没登录、断网)下次进页再试。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn restore(
    ui: &MainWindow,
    deck: &Deck,
) {
    if deck.restored.replace(true) || !idle(deck) {
        return;
    }
    let deck = deck.clone();
    let weak = ui.as_weak();
    let _ = slint::spawn_local(async move {
        let device = crate::sync::link::local_device_id();
        let head = match api::device_queue(&device).await {
            Ok(Some(head)) => head,
            Ok(None) => return,
            Err(error) => {
                log::debug!("找回上次的队列没成: {error}");
                deck.restored.set(false);
                return;
            }
        };
        let entries = match api::fetch_queue(
            head.queue_id,
            head.revision,
        )
        .await
        {
            Ok(entries) => entries,
            Err(error) => {
                log::debug!("取上次的队列没成: {error}");
                deck.restored.set(false);
                return;
            }
        };
        let Some(resume) = resume_from(&head, entries)
        else {
            return;
        };
        // 等网络的这几秒里用户可能已经点了歌、或进了组:那就不恢复了
        let Some(ui) = weak.upgrade() else { return };
        if !idle(&deck) {
            return;
        }
        apply(
            &ui,
            &deck,
            head.queue_id,
            head.revision,
            resume,
        );
    });
}

/// 手上没歌、也不在组里。
fn idle(deck: &Deck) -> bool {
    !deck.group.is_member()
        && deck.queue.borrow().current().is_none()
}

/// 装回队列,停在那一首的那一秒上。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn apply(
    ui: &MainWindow,
    deck: &Deck,
    queue_id: i64,
    revision: i64,
    resume: Resume,
) {
    log::info!(
        "接着上次:第 {} 首,{:?}",
        resume.index + 1,
        resume.position
    );
    deck.queue
        .borrow_mut()
        .replace(resume.tracks, resume.index);
    if let Some(order) = resume.order {
        deck.queue.borrow_mut().restore_order(order, true);
    }
    deck.execution.adopt(
        queue_id,
        revision,
        resume.entry_ids,
    );
    deck.start_at.set(Some(Start {
        at: resume.position,
        playing: false,
    }));
    play_current(ui, deck);
    // 停下那一段接着算:同一行起播事件、累计接着往上加,跨着重启听够了也算听过。
    // 要在 `play_current` 之后 —— 它会把上一段的事件行与时钟清掉。
    if let Some(play) = resume.play {
        let duration_ms = deck
            .queue
            .borrow()
            .current()
            .map_or(0, |track| track.duration_ms);
        deck.current_play.replace(Some(CurrentPlay {
            id: play.play_event_id,
            duration_ms,
            continued: true,
        }));
        deck.listen_clock.set(ListenClock::resumed(
            play.listened_ms,
            resume.position,
        ));
    }
    mark_sync(ui, deck);
    push_media(ui, deck);
}
