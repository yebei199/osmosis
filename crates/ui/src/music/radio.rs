//! 电台(#159):私人 FM 与心动模式起播,以及队列剩最后一首时续一批。
//!
//! 电台不是另一套队列:起播就是一次普通点歌(整批进队列),续取是往同一批
//! 队尾追加,再把新的一版发布到服务端。「还是不是电台在放」靠队列的批号认 ——
//! 用户点了别的歌,队列换批,电台就不再往里续,不必另外记一个开关去关它。
//! 例外是在电台区里点的(#166):那一批仍算电台的,选着的筛选成了电台的口味,
//! 续进来的歌照它过滤(见 [`adopt`])。
//!
//! 在组里时点歌走组意图,本机队列不换批,电台因此不续(一起听不做特殊处理)。

use app_core::FacetPickDto;

use super::*;
use crate::Shell;
use crate::runtime::trace::Action;

/// 续取失败或一首新歌都没续上之后,隔多久再试。每秒一趟的轮询不能每秒问一次平台。
const RETRY_AFTER_MS: u64 = 30_000;

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
    /// 续歌带的筛选。电台区里选了 chip 再点歌时记下(#166),开新电台时清空。
    filter: Vec<FacetPickDto>,
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
    let before = deck.queue.borrow().batch();
    dispatch(ui, deck, Intent::Play { tracks, index: 0 });
    let after = deck.queue.borrow().batch();
    // 没换批就不是电台在放:组里走了组意图,或者这一下被连点去重挡了
    if after != before {
        let mut state = deck.radio.inner.borrow_mut();
        state.mode = Some(mode);
        state.batch = after;
        state.filter.clear();
        state.retry_at_ms = 0;
    }
}

/// 在电台区点了一首(#166):换出来的这一批仍是电台的,选着的筛选记下来,
/// 续歌时带给服务端。`before` 是点之前的批号;没换批(组里走了组意图、
/// 连点被去重)或者电台从没开过,就什么都不做。
pub(super) fn adopt(deck: &Deck, before: u64) {
    let after = deck.queue.borrow().batch();
    if after == before
        || deck.views.current_source()
            != Some(ViewSource::Radio)
    {
        return;
    }
    let filter = app_core::facets::picks(
        deck.facets.borrow().chosen(),
    );
    let mut state = deck.radio.inner.borrow_mut();
    if state.mode.is_none() {
        return;
    }
    state.batch = after;
    state.filter = filter;
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

/// 队列还是电台起播的那一批。
fn owns_batch(deck: &Deck) -> bool {
    let state = deck.radio.inner.borrow();
    state.mode.is_some()
        && state.batch == deck.queue.borrow().batch()
}

/// 每秒一趟:电台的队列只剩最后一首就续一批。
pub(super) fn top_up(ui: &MainWindow, deck: &Deck) {
    let now_ms = crate::sync::group::now_ms();
    let turn = {
        let state = deck.radio.inner.borrow();
        RadioTurn {
            owns_batch: owns_batch(deck),
            remaining: deck.queue.borrow().remaining(),
            pulling: state.pulling,
            now_ms,
            retry_at_ms: state.retry_at_ms,
        }
    };
    if !radio_due(&turn) {
        return;
    }
    let Some(mode) = next_mode(deck) else { return };
    let filter = {
        let mut state = deck.radio.inner.borrow_mut();
        state.pulling = true;
        state.filter.clone()
    };

    let batch = deck.queue.borrow().batch();
    let deck = deck.clone();
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let found = api::radio(&mode, &filter).await;
        let mut added = 0;
        match found {
            // 等的这几秒里用户点了别的歌:这一批不是它的了,扔掉
            Ok(found)
                if deck.queue.borrow().batch() == batch =>
            {
                added = deck
                    .queue
                    .borrow_mut()
                    .append(found.tracks);
                log::info!("电台续了 {added} 首");
            }
            Ok(_) => {}
            Err(error) => {
                log::warn!("电台续取失败: {error}")
            }
        }
        {
            let mut state = deck.radio.inner.borrow_mut();
            state.pulling = false;
            if added == 0 {
                state.retry_at_ms = now_ms + RETRY_AFTER_MS;
            }
        }
        if added > 0
            && let Some(ui) = weak.upgrade()
        {
            republish(&ui, &deck);
        }
    })
    .expect("event loop must be running");
}

/// 续这一批用什么:私人 FM 照旧;心动以队尾那首为种子,往下接着推。
fn next_mode(deck: &Deck) -> Option<api::RadioMode> {
    let mode = deck.radio.inner.borrow().mode.clone()?;
    Some(match mode {
        api::RadioMode::Fm => api::RadioMode::Fm,
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
