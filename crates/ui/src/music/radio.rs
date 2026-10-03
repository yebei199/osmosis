//! 电台(#159):私人 FM 与心动模式起播,以及队列剩最后一首时续一批。
//!
//! 电台不是另一套队列:起播就是一次普通点歌(整批进队列),续取是往同一批
//! 队尾追加,再把新的一版发布到服务端。「还是不是电台在放」靠队列的批号认 ——
//! 用户点了别的歌,队列换批,电台就不再往里续,不必另外记一个开关去关它。
//! 例外是在电台区里点的(#166):那一批仍算电台的,选着的筛选成了电台的口味,
//! 续进来的歌照它过滤(见 [`adopt`])。
//!
//! 在组里时(#165)起播走组意图,「还是不是电台在放」改认组队列的版本:起播的应答里
//! 那一版是电台的,续取走组意图追加进去、换到续上的那一版;有人点了别的歌,组队列换了
//! 一版,电台就不再往里续。组里只看得见下一首,所以在放最后一首时才续。
//! 独奏电台切输出入组时,只有服务端采用同一 seed 且本机未换批才接管组队列。
//! 续取仍由发起电台的设备负责,该设备下线或挂起时不会由其他成员代续。

use app_core::FacetPickDto;

use super::*;
use crate::Shell;
use crate::runtime::trace::Action;

/// 续取失败或一首新歌都没续上之后,隔多久再试。每秒一趟的轮询不能每秒问一次平台。
const RETRY_AFTER_MS: u64 = 30_000;

/// 带着筛选续歌,服务端问满上限仍一首不剩时的提示(#166)。
const DRY_NOTICE: &str = "按当前筛选暂时找不到新歌";

/// 电台此刻的账。
#[derive(Clone, Default)]
pub(super) struct Radio {
    inner: Rc<RefCell<State>>,
}

#[derive(Default)]
struct State {
    /// 在放哪种电台。`None` 是从没开过。
    mode: Option<api::RadioMode>,
    /// 电台起播的那一批的批号(`Queue::batch`)。
    batch: u64,
    /// 在组里时电台的那一版组队列 `(queue_id, revision)`(#165)。
    shared: Option<(i64, i64)>,
    /// 续歌带的筛选。电台区里选了 chip 再点歌时记下(#166),开新电台时清空。
    filter: Vec<FacetPickDto>,
    /// 「按当前筛选找不到新歌」已经说过了。续上一首之前不再说第二遍 ——
    /// 冷却每 30 秒试一次,每次都弹就成了噪音。
    told_dry: bool,
    pulling: bool,
    retry_at_ms: u64,
}

/// 接上播放页的「从这首开电台」:以正在放的那首为种子开心动模式。
pub(super) fn bind(ui: &MainWindow, deck: &Deck) {
    let deck = deck.clone();
    let weak = ui.as_weak();
    ui.global::<crate::Player>().on_radio_from(move |id| {
        let Some(ui) = weak.upgrade() else { return };
        if id.is_empty() {
            return;
        }
        start(
            &ui,
            &deck,
            api::RadioMode::Heart {
                seed: id.to_string(),
            },
        );
    });
}

/// 开电台:取一批,拿到就整批起播。
///
/// 正在电台区就摆进列表;从播放页开的心动模式在后台进电台区自己那份,
/// 不把用户正在看的列表换掉。
pub(super) fn start(
    ui: &MainWindow,
    deck: &Deck,
    mode: api::RadioMode,
) {
    let weak = ui.as_weak();
    let request = {
        let weak = weak.clone();
        let deck = deck.clone();
        async move {
            let found = api::radio(&mode, &[]).await?;
            if let Some(ui) = weak.upgrade() {
                begin(
                    &ui,
                    &deck,
                    mode,
                    found.tracks.clone(),
                );
            }
            Ok(found)
        }
    };
    let action = Action::begin("radio");
    let on_screen = Section::from_index(
        ui.global::<Shell>().get_music_section(),
    ) == Section::Radio
        && ui
            .global::<crate::Library>()
            .get_open_playlist_name()
            .is_empty();
    if on_screen {
        fetch_into(
            &weak,
            deck,
            action,
            ViewSource::Radio,
            request,
        );
    } else {
        let ticket = deck
            .views
            .begin_in_background(ViewSource::Radio);
        land(
            &weak,
            deck,
            action,
            ticket,
            async { None },
            request,
        );
    }
}

/// 电台区被点开:电台已经在放私人 FM 就只摆出来,否则开私人 FM。
pub(super) fn open(ui: &MainWindow, deck: &Deck) {
    if owns_batch(deck)
        && deck.radio.inner.borrow().mode
            == Some(api::RadioMode::Fm)
    {
        show_section(
            ui,
            deck,
            ui.global::<Shell>().get_music_section(),
        );
        return;
    }
    start(ui, deck, api::RadioMode::Fm);
}

/// 拿到的这一批起播,并记下它是电台的。
pub(super) fn begin(
    ui: &MainWindow,
    deck: &Deck,
    mode: api::RadioMode,
    tracks: Vec<TrackDto>,
) {
    if tracks.is_empty() {
        crate::notice::show(
            ui,
            "电台这会儿没有没听过的新歌".to_owned(),
        );
        return;
    }
    if in_dead_group(deck) {
        leave_dead_group(ui, deck, move |ui, deck| {
            begin(ui, deck, mode, tracks);
        });
        return;
    }
    if deck.group.is_member() {
        let radio = deck.radio.clone();
        deck.group.play_then(
            ui,
            tracks,
            0,
            Box::new(move |queue| {
                let queue = queue.ok().flatten();
                if queue.is_some() {
                    radio.inner.replace(State {
                        mode: Some(mode),
                        shared: queue,
                        ..State::default()
                    });
                }
            }),
        );
        return;
    }
    let before = deck.queue.borrow().batch();
    dispatch(ui, deck, Intent::Play { tracks, index: 0 });
    let after = deck.queue.borrow().batch();
    // 没换批就不是电台在放:组里走了组意图,或者这一下被连点去重挡了
    if after != before {
        let mut state = deck.radio.inner.borrow_mut();
        state.mode = Some(mode);
        state.batch = after;
        state.shared = None;
        state.filter.clear();
        state.told_dry = false;
        state.retry_at_ms = 0;
    }
}

/// 在电台区点了一首(#166):换出来的这一批仍是电台的,选着的筛选记下来,
/// 续歌时带给服务端。`before` 是点之前的批号。
///
/// 没换批分两种:点的正是在放的那首(连点去重挡掉了),电台仍在放这一批 ——
/// 筛选照样记下,不然选了 chip 点在放的那首,续进来的却不筛;组里走了组意图、
/// 本机队列不归电台 —— 什么都不做。电台从没开过也什么都不做。
pub(super) fn adopt(deck: &Deck, before: u64) {
    if deck.views.current_source()
        != Some(ViewSource::Radio)
    {
        return;
    }
    let after = deck.queue.borrow().batch();
    let filter = app_core::facets::picks(
        deck.facets.borrow().chosen(),
    );
    let mut state = deck.radio.inner.borrow_mut();
    let foreign = after == before && state.batch != after;
    if state.mode.is_none() || foreign {
        log::info!(
            "电台区点歌,电台不接:没开过或这一批不归它"
        );
        return;
    }
    log::info!("电台接下这一批,筛选 {} 条", filter.len());
    state.batch = after;
    state.filter = filter;
    state.told_dry = false;
    state.retry_at_ms = 0;
}

/// 电台还在放的话,它续歌带的筛选;不在放是 `None`。
#[cfg(test)]
pub(super) fn taste(
    deck: &Deck,
) -> Option<Vec<FacetPickDto>> {
    owns_batch(deck)
        .then(|| deck.radio.inner.borrow().filter.clone())
}

/// 独奏电台的 seed 被组采用后接管那一版;失败、换批和其他队列都不接。
pub(in crate::music) fn seed_handoff(
    deck: &Deck,
    seed: Option<&app_core::GroupSeedDto>,
) -> crate::sync::group::Then {
    let Some(seed) = seed else {
        return Box::new(|_| {});
    };
    let seeded = (seed.queue_id, seed.revision);
    let batch = deck.queue.borrow().batch();
    let queue = deck.queue.clone();
    let radio = deck.radio.clone();
    Box::new(move |reply| {
        if reply.ok().flatten() != Some(seeded)
            || queue.borrow().batch() != batch
        {
            return;
        }
        let mut state = radio.inner.borrow_mut();
        if state.mode.is_some()
            && state.batch == batch
            && state.shared.is_none()
        {
            state.shared = Some(seeded);
        }
    })
}

/// 队列还是电台起播的那一批;在组里是组此刻那一版还是电台的。
fn owns_batch(deck: &Deck) -> bool {
    let state = deck.radio.inner.borrow();
    if state.mode.is_none() {
        return false;
    }
    if deck.group.is_member() {
        state.shared.is_some()
            && state.shared == shared_now(deck)
    } else {
        state.batch == deck.queue.borrow().batch()
    }
}

/// 组此刻那一版组队列。
fn shared_now(deck: &Deck) -> Option<(i64, i64)> {
    deck.group.now().map(|now| (now.queue_id, now.revision))
}

/// 当前这首之后还排着几首。组里只看得见下一首:在放最后一首是 0,否则按 1 之外算。
fn remaining(deck: &Deck) -> usize {
    if !deck.group.is_member() {
        return deck.queue.borrow().remaining();
    }
    match deck.group.now() {
        Some(now) if now.playing && now.next.is_none() => 0,
        _ => usize::MAX,
    }
}

/// 每秒一趟:电台的队列只剩最后一首就续一批。
pub(super) fn top_up(ui: &MainWindow, deck: &Deck) {
    let now_ms = crate::sync::group::now_ms();
    if !due(deck, now_ms) {
        return;
    }
    let Some(mode) = next_mode(deck) else { return };
    let filter = {
        let mut state = deck.radio.inner.borrow_mut();
        state.pulling = true;
        state.filter.clone()
    };

    if deck.group.is_member() {
        top_up_shared(ui, deck, mode, filter, now_ms);
        return;
    }
    let batch = deck.queue.borrow().batch();
    let deck = deck.clone();
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let found = api::radio(&mode, &filter).await;
        let mut added = 0;
        let mut dry = false;
        match found {
            // 等的这几秒里用户点了别的歌:这一批不是它的了,扔掉
            Ok(found)
                if deck.queue.borrow().batch() == batch =>
            {
                added = deck
                    .queue
                    .borrow_mut()
                    .append(found.tracks);
                dry = added == 0 && !filter.is_empty();
                log::info!("电台续了 {added} 首");
            }
            Ok(_) => {}
            Err(error) => {
                log::warn!("电台续取失败: {error}")
            }
        }
        if let Some(ui) = weak.upgrade() {
            settle(&ui, &deck, added, dry, now_ms);
        }
        if added > 0
            && let Some(ui) = weak.upgrade()
        {
            republish(&ui, &deck);
        }
    })
    .expect("event loop must be running");
}

/// 此刻该不该续一批。
pub(super) fn due(deck: &Deck, now_ms: u64) -> bool {
    let state = deck.radio.inner.borrow();
    radio_due(&RadioTurn {
        owns_batch: owns_batch(deck),
        remaining: remaining(deck),
        pulling: state.pulling,
        now_ms,
        retry_at_ms: state.retry_at_ms,
    })
}

/// 在组里续:取到的歌走组意图追加进电台那一版组队列,电台换到续上的那一版(#165)。
fn top_up_shared(
    ui: &MainWindow,
    deck: &Deck,
    mode: api::RadioMode,
    filter: Vec<FacetPickDto>,
    now_ms: u64,
) {
    let deck = deck.clone();
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let found = api::radio(&mode, &filter).await;
        let Some(ui) = weak.upgrade() else { return };
        let (tracks, dry) = match found {
            // 等的这几秒里有人点了别的歌:组队列不是电台的了,扔掉
            Ok(found) if owns_batch(&deck) => {
                let dry = found.tracks.is_empty()
                    && !filter.is_empty();
                (found.tracks, dry)
            }
            Ok(_) => (Vec::new(), false),
            Err(error) => {
                log::warn!("电台续取失败: {error}");
                (Vec::new(), false)
            }
        };
        let queue = deck.radio.inner.borrow().shared;
        let (Some(queue), false) =
            (queue, tracks.is_empty())
        else {
            settle(&ui, &deck, 0, dry, now_ms);
            return;
        };
        let added = tracks.len();
        let radio = deck.radio.clone();
        let settling = deck.clone();
        deck.group.append(
            &ui,
            queue,
            tracks,
            Box::new(move |extended| {
                let extended = extended.ok().flatten();
                let added = if extended.is_some() {
                    radio.inner.borrow_mut().shared =
                        extended;
                    log::info!(
                        "电台往组队列续了 {added} 首"
                    );
                    added
                } else {
                    0
                };
                if let Some(ui) = weak.upgrade() {
                    settle(
                        &ui, &settling, added, false,
                        now_ms,
                    );
                }
            }),
        );
    })
    .expect("event loop must be running");
}

/// 一趟续取落定:放下「在路上」,没续上就冷却,筛选续不出新歌时说一次。
fn settle(
    ui: &MainWindow,
    deck: &Deck,
    added: usize,
    dry: bool,
    now_ms: u64,
) {
    let tell_dry = {
        let mut state = deck.radio.inner.borrow_mut();
        state.pulling = false;
        if added == 0 {
            state.retry_at_ms = now_ms + RETRY_AFTER_MS;
        } else {
            state.told_dry = false;
        }
        let tell = dry && !state.told_dry;
        state.told_dry |= dry;
        tell
    };
    if tell_dry {
        crate::notice::show(ui, DRY_NOTICE.to_owned());
    }
}

/// 续这一批用什么:私人 FM 照旧;心动以队尾那首为种子,往下接着推。
fn next_mode(deck: &Deck) -> Option<api::RadioMode> {
    let mode = deck.radio.inner.borrow().mode.clone()?;
    Some(match mode {
        api::RadioMode::Fm => api::RadioMode::Fm,
        // 组里在放最后一首时才续,那一首就是队尾
        api::RadioMode::Heart { .. }
            if deck.group.is_member() =>
        {
            api::RadioMode::Heart {
                seed: deck.group.now()?.track.id,
            }
        }
        api::RadioMode::Heart { .. } => {
            api::RadioMode::Heart {
                seed: deck
                    .queue
                    .borrow()
                    .tracks()
                    .last()?
                    .id
                    .clone(),
            }
        }
    })
}

/// 续上之后:新的一版发布到服务端,电台区的列表跟着长。
fn republish(ui: &MainWindow, deck: &Deck) {
    let (tracks, index) = {
        let queue = deck.queue.borrow();
        (queue.tracks().to_vec(), queue.index())
    };
    publish_local_queue(ui, deck, tracks.clone(), index);

    let ticket =
        deck.views.begin_in_background(ViewSource::Radio);
    let landing = deck.views.accept(
        &ticket,
        TracksDto {
            tracks,
            unavailable: 0,
            hidden: 0,
        },
        true,
    );
    // 同一视图续长,不是换视图:走 show 而不是 project,选着的筛选与折叠不清(#160)
    if landing == Landing::Current
        && let Some(found) =
            deck.views.show(ViewSource::Radio).tracks
    {
        ui.global::<Player>().set_tracks_loading(false);
        show(ui, deck, found);
    }
}
