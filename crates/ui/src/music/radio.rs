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
//! 一版,电台就不再往里续。在电台区点的那一下例外(#182):服务端换出来的那一版仍归电台
//! (见 [`follow`])。组按服务端的实际播放次序末尾标记续取,循环预告不影响续取。
//! 独奏电台切输出入组时,只有服务端采用同一 seed 且本机未换批才接管组队列。
//! 续取仍由发起电台的设备负责,该设备下线或挂起时不会由其他成员代续。
//!
//! 私人 FM 读账号那一份共享电台歌单(#186):电台区摆的是服务端那份,「加载新歌」
//! 让服务端追加一批,信令说歌单变了就重取。续歌先从共享歌单里挑还没交给播放的,
//! 挑不出再让服务端加载一批。听过的不在歌单主列表里,切到「已听过」才摆它们。
//! 心动模式仍按设备各自拉,不进共享歌单。

use std::collections::HashSet;

use app_core::facets::Chosen;
use app_core::{FacetPickDto, RadioListDto};

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
    /// 续歌带的筛选在本机的样子:从共享歌单里挑歌时按它过(#186)。
    chosen: Chosen,
    /// 账号的共享电台歌单最近一次取到的那份(#186)。
    list: RadioListDto,
    /// 电台区在摆「已听过」。
    heard_view: bool,
    /// 「加载新歌」在路上。
    loading: bool,
    /// 私人 FM 已经交给播放的曲目 id。续歌只从共享歌单里挑还没交出去的。
    handed: HashSet<String>,
    /// 组里在放的、已经为它重取过歌单的那首。组的起播由服务端记,不经 `/played`,
    /// 于是没有信令说它听过了,本机见它开播就自己重取一次。
    refreshed_for: Option<String>,
}

impl State {
    /// 开一轮新的电台:之前那一轮的续歌账作废,共享歌单与「已听过」开关照旧。
    fn restart(
        &mut self,
        mode: api::RadioMode,
        tracks: &[TrackDto],
    ) {
        self.mode = Some(mode);
        self.batch = 0;
        self.shared = None;
        self.filter.clear();
        self.chosen.clear();
        self.told_dry = false;
        self.pulling = false;
        self.retry_at_ms = 0;
        self.handed = tracks
            .iter()
            .map(|track| track.id.clone())
            .collect();
    }
}

/// 接上播放页的「从这首开电台」:以正在放的那首为种子开心动模式;
/// 以及电台区的「加载新歌」「已听过」和信令来的「歌单变了」(#186)。
pub(super) fn bind(ui: &MainWindow, deck: &Deck) {
    let player = ui.global::<crate::Player>();
    {
        let deck = deck.clone();
        let weak = ui.as_weak();
        player.on_radio_load_more(move || {
            if let Some(ui) = weak.upgrade() {
                load_more(&ui, &deck);
            }
        });
    }
    {
        let deck = deck.clone();
        let weak = ui.as_weak();
        player.on_radio_toggle_heard(move || {
            let Some(ui) = weak.upgrade() else { return };
            let heard = {
                let mut state =
                    deck.radio.inner.borrow_mut();
                state.heard_view = !state.heard_view;
                state.heard_view
            };
            ui.global::<crate::Player>()
                .set_radio_heard(heard);
            show_list(&ui, &deck);
        });
    }
    {
        let deck = deck.clone();
        let weak = ui.as_weak();
        player.on_radio_changed(move || {
            if let Some(ui) = weak.upgrade() {
                refresh(&ui, &deck);
            }
        });
    }
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
            if mode != api::RadioMode::Fm {
                let found = api::radio(&mode, &[]).await?;
                if let Some(ui) = weak.upgrade() {
                    begin(
                        &ui,
                        &deck,
                        mode,
                        found.tracks.clone(),
                    );
                }
                return Ok(found);
            }
            // 私人 FM 放的是共享歌单里还没听的;一首都没有就先让服务端加载一批
            let mut list = api::radio_list().await?;
            if list.tracks.is_empty() {
                list = api::radio_more(&[]).await?;
            }
            let tracks = list.tracks.clone();
            remember(&deck, list);
            if let Some(ui) = weak.upgrade() {
                begin(&ui, &deck, mode, tracks);
            }
            Ok(shown(&deck))
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
        // 别的设备可能加过新歌、听过几首:摆出手上那份的同时重取一次
        refresh(ui, deck);
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
        let handed = tracks.clone();
        deck.group.play_then(
            ui,
            tracks,
            0,
            Box::new(move |queue| {
                let queue = queue.ok().flatten();
                if queue.is_some() {
                    let mut state =
                        radio.inner.borrow_mut();
                    state.restart(mode, &handed);
                    state.shared = queue;
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
        let handed = deck.queue.borrow().tracks().to_vec();
        let mut state = deck.radio.inner.borrow_mut();
        state.restart(mode, &handed);
        state.batch = after;
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
    let chosen = deck.facets.borrow().chosen().clone();
    let filter = app_core::facets::picks(&chosen);
    let mut state = deck.radio.inner.borrow_mut();
    let foreign = after == before && state.batch != after;
    // 组里由 [`follow`] 接,本机队列与电台无关
    if state.mode.is_none()
        || foreign
        || deck.group.is_member()
    {
        log::info!(
            "电台区点歌,电台不接:没开过或这一批不归它"
        );
        return;
    }
    log::info!("电台接下这一批,筛选 {} 条", filter.len());
    state.batch = after;
    state.filter = filter;
    state.chosen = chosen;
    state.told_dry = false;
    state.retry_at_ms = 0;
    let queued = deck.queue.borrow();
    state.handed.extend(
        queued
            .tracks()
            .iter()
            .map(|track| track.id.clone()),
    );
}

/// 组里点歌的应答(#182):在电台区点的,换出来的那一版仍归电台,选着的筛选记下来;
/// 别处点的不接,组换了一版电台就停续。列表与组队列逐首一致时服务端沿用原版,
/// 筛过、分过组、或续歌后本机列表没跟着长,都会另起一版。
pub(in crate::music) fn follow(
    deck: &Deck,
) -> crate::sync::group::Then {
    if deck.views.current_source()
        != Some(ViewSource::Radio)
        || deck.radio.inner.borrow().mode.is_none()
    {
        return Box::new(|_| {});
    }
    let filter = app_core::facets::picks(
        deck.facets.borrow().chosen(),
    );
    let radio = deck.radio.clone();
    Box::new(move |reply| {
        let Some(queue) = reply.ok().flatten() else {
            return;
        };
        log::info!(
            "电台接下组里这一版,筛选 {} 条",
            filter.len()
        );
        let mut state = radio.inner.borrow_mut();
        state.shared = Some(queue);
        state.filter = filter;
        state.told_dry = false;
        state.retry_at_ms = 0;
    })
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

/// 组按服务端实际次序末尾标记判队尾;旧后端继续用下一首预告兜底。
fn remaining(deck: &Deck) -> usize {
    if !deck.group.is_member() {
        return deck.queue.borrow().remaining();
    }
    match deck.group.now() {
        Some(now)
            if now.playing
                && (now.at_end || now.next.is_none()) =>
        {
            0
        }
        _ => usize::MAX,
    }
}

/// 每秒一趟:电台的队列只剩最后一首就续一批。
pub(super) fn top_up(ui: &MainWindow, deck: &Deck) {
    if deck.group.is_member() {
        follow_group_play(ui, deck);
    }
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
        let found = next_batch(&deck, &mode, &filter).await;
        let mut added = 0;
        let mut dry = false;
        match found {
            // 等的这几秒里用户点了别的歌:这一批不是它的了,扔掉
            Ok(found)
                if deck.queue.borrow().batch() == batch =>
            {
                hand(&deck, &found);
                added =
                    deck.queue.borrow_mut().append(found);
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
            republish(&ui, &deck, &mode);
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
        let found = next_batch(&deck, &mode, &filter).await;
        let Some(ui) = weak.upgrade() else { return };
        let (tracks, dry) = match found {
            // 等的这几秒里有人点了别的歌:组队列不是电台的了,扔掉
            Ok(found) if owns_batch(&deck) => {
                let dry =
                    found.is_empty() && !filter.is_empty();
                hand(&deck, &found);
                (found, dry)
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
/// 私人 FM 的电台区摆的是共享歌单,换成它;心动模式摆的是队列。
fn republish(
    ui: &MainWindow,
    deck: &Deck,
    mode: &api::RadioMode,
) {
    let (tracks, index) = {
        let queue = deck.queue.borrow();
        (queue.tracks().to_vec(), queue.index())
    };
    publish_local_queue(ui, deck, tracks.clone(), index);
    if *mode == api::RadioMode::Fm {
        show_list(ui, deck);
        return;
    }

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

/// 续一批要的歌。私人 FM 从共享歌单里挑还没交给播放的(按续歌的筛选过),
/// 挑不出再让服务端加载一批(#186);心动模式照旧向服务端要。
async fn next_batch(
    deck: &Deck,
    mode: &api::RadioMode,
    filter: &[FacetPickDto],
) -> Result<Vec<TrackDto>, api::ApiError> {
    if *mode != api::RadioMode::Fm {
        return Ok(api::radio(mode, filter).await?.tracks);
    }
    let (handed, chosen) = {
        let state = deck.radio.inner.borrow();
        (state.handed.clone(), state.chosen.clone())
    };
    let mut list = api::radio_list().await?;
    let mut fresh =
        radio_unhanded(&list.tracks, &handed, &chosen);
    if fresh.is_empty() {
        list = api::radio_more(filter).await?;
        fresh =
            radio_unhanded(&list.tracks, &handed, &chosen);
    }
    remember(deck, list);
    Ok(fresh)
}

/// 这几首交给播放了,续歌不再挑它们。
fn hand(deck: &Deck, tracks: &[TrackDto]) {
    deck.radio.inner.borrow_mut().handed.extend(
        tracks.iter().map(|track| track.id.clone()),
    );
}

/// 记下刚取到的共享歌单。
fn remember(deck: &Deck, list: RadioListDto) {
    deck.radio.inner.borrow_mut().list = list;
}

/// 电台区此刻该摆的那一半:「已听过」或还没听的。
fn shown(deck: &Deck) -> TracksDto {
    let state = deck.radio.inner.borrow();
    let tracks = if state.heard_view {
        state.list.heard.clone()
    } else {
        state.list.tracks.clone()
    };
    TracksDto {
        tracks,
        unavailable: 0,
        hidden: 0,
    }
}

/// 把共享歌单摆进电台区那份视图;正看着电台区就当场换上。
/// 心动模式在放时电台区归它,不动。
fn show_list(ui: &MainWindow, deck: &Deck) {
    if matches!(
        deck.radio.inner.borrow().mode,
        Some(api::RadioMode::Heart { .. })
    ) && owns_batch(deck)
    {
        return;
    }
    let ticket =
        deck.views.begin_in_background(ViewSource::Radio);
    let landing =
        deck.views.accept(&ticket, shown(deck), true);
    // 同一视图换内容,不是换视图:走 show 而不是 project,选着的筛选与折叠不清(#160)
    if landing == Landing::Current
        && let Some(found) =
            deck.views.show(ViewSource::Radio).tracks
    {
        ui.global::<Player>().set_tracks_loading(false);
        show(ui, deck, found);
    }
}

/// 重取共享歌单再摆出来。失败只记日志:下一次信令或点开电台区还会再取。
fn refresh(ui: &MainWindow, deck: &Deck) {
    let deck = deck.clone();
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        match api::radio_list().await {
            Ok(list) => {
                remember(&deck, list);
                if let Some(ui) = weak.upgrade() {
                    show_list(&ui, &deck);
                }
            }
            Err(error) => {
                log::warn!("电台歌单重取失败: {error}")
            }
        }
    })
    .expect("event loop must be running");
}

/// 「加载新歌」:服务端从私人 FM 拉一批追加进共享歌单。别的设备由信令通知,
/// 本机直接用应答里那份。
fn load_more(ui: &MainWindow, deck: &Deck) {
    {
        let mut state = deck.radio.inner.borrow_mut();
        if state.loading {
            return;
        }
        state.loading = true;
    }
    ui.global::<Player>().set_radio_loading(true);
    let deck = deck.clone();
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let found = api::radio_more(&[]).await;
        deck.radio.inner.borrow_mut().loading = false;
        let Some(ui) = weak.upgrade() else { return };
        ui.global::<Player>().set_radio_loading(false);
        match found {
            Ok(list) => {
                let before = deck
                    .radio
                    .inner
                    .borrow()
                    .list
                    .tracks
                    .len();
                if list.tracks.len() <= before {
                    crate::notice::show(
                        &ui,
                        "电台这会儿没有没听过的新歌"
                            .to_owned(),
                    );
                }
                remember(&deck, list);
                show_list(&ui, &deck);
            }
            Err(error) => {
                log::warn!("加载新歌失败: {error}");
                crate::notice::show(
                    &ui,
                    format!("加载新歌失败: {error}"),
                );
            }
        }
    })
    .expect("event loop must be running");
}

/// 组里开播了共享歌单里的一首(#186):服务端记了这次播放,本机重取一次,
/// 让它挪进「已听过」。每首只取一次。
fn follow_group_play(ui: &MainWindow, deck: &Deck) {
    let Some(now) = deck.group.now() else { return };
    let due = {
        let mut state = deck.radio.inner.borrow_mut();
        let listed = state
            .list
            .tracks
            .iter()
            .any(|track| track.id == now.track.id);
        let due = listed
            && state.refreshed_for.as_deref()
                != Some(now.track.id.as_str());
        if due {
            state.refreshed_for =
                Some(now.track.id.clone());
        }
        due
    };
    if due {
        refresh(ui, deck);
    }
}
