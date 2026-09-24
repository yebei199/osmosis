//! 队列页:摆出**当前执行副本**的那一份队列(#109 AC-15)。
//!
//! 一条路径两种情形,不写成两套:
//!
//! - 输出在本机:队列就在手上(`deck.queue`),条目号在 `deck.execution` 里。
//! - 输出在别的设备:被控端每秒报来 `queue_id`/`revision`,按它去 HTTP 上拉
//!   一份**只读显示缓存**(`docs/adr/0031` 三)。遥控器持有的不是第二份真相
//!   —— 它不拿这份去决定下一首,只用来画。
//!
//! 点一行的去向也分这两种,而且都落在 `entry_id` 上:本机直接跳,遥控发一条
//! 带那个 `entry_id` 的意图。**不按下标**:队列允许同一首歌出现多次,
//! 按曲目或下标认的话,点第二次出现的那一条会放到第一条上去。
//!
//! 队列还没同步到服务端时(`docs/adr/0031` 八)没有真的条目号,这里用下标
//! 现编一个。它只在本机这一次有效,而本机那条路本来就只要「第几首」。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use app_core::TrackDto;
use slint::{ComponentHandle, VecModel};

use crate::music::*;
use crate::{MainWindow, TrackRow, Viz};

/// 遥控时拉下来的那份只读显示缓存。
///
/// 只在队列页要画的时候取,取到就留着 —— 队列没变就零次重传
/// (`docs/adr/0031` 三)。
#[derive(Clone, Default)]
pub(in crate::music) struct QueueMirror {
    inner: Rc<RefCell<Option<Mirrored>>>,
    /// 正在取的那一版。每秒那一趟看见它就不再发第二次(#137 ⑥)。
    fetching: Rc<Cell<Option<(i64, i64)>>>,
    /// 队列页的行此刻摆的是哪一份。没变就不重建(#137 ⑥)。
    shown: Rc<Cell<Option<Shown>>>,
}

/// 队列页那几行是按什么建的:本机是「第几批 + 同步到哪一版」,遥控是被控端那一版。
/// 两者都不变,行就不必重建 —— 当前是哪一条是另一个标量,单独更新。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Local {
        batch: u64,
        queue_id: Option<i64>,
        revision: Option<i64>,
    },
    Remote {
        queue_id: i64,
        revision: i64,
    },
    Unsynced,
}

struct Mirrored {
    queue_id: i64,
    revision: i64,
    entries: Vec<(i64, TrackDto)>,
}

impl QueueMirror {
    /// 手上这份是不是正好是要的那一版。
    fn holds(&self, queue_id: i64, revision: i64) -> bool {
        self.inner.borrow().as_ref().is_some_and(|held| {
            held.queue_id == queue_id
                && held.revision == revision
        })
    }

    fn put(
        &self,
        queue_id: i64,
        revision: i64,
        entries: Vec<(i64, TrackDto)>,
    ) {
        self.fetch_failed(queue_id, revision);
        *self.inner.borrow_mut() = Some(Mirrored {
            queue_id,
            revision,
            entries,
        });
    }

    /// 要不要为这一版发一次取数:手上已有、或者同一版正在路上,都不发。
    fn begin_fetch(
        &self,
        queue_id: i64,
        revision: i64,
    ) -> bool {
        let wanted = Some((queue_id, revision));
        if self.holds(queue_id, revision)
            || self.fetching.get() == wanted
        {
            return false;
        }
        self.fetching.set(wanted);
        true
    }

    /// 这一版的取数结束了(成败都算),下一秒可以再来。
    fn fetch_failed(&self, queue_id: i64, revision: i64) {
        if self.fetching.get() == Some((queue_id, revision))
        {
            self.fetching.set(None);
        }
    }

    /// 行此刻已经按 `key` 建好了没有;没有就记下「这就去建」。
    fn needs_rows(&self, key: Shown) -> bool {
        self.shown.replace(Some(key)) != Some(key)
    }

    fn rows(&self) -> Vec<(i64, TrackDto)> {
        self.inner
            .borrow()
            .as_ref()
            .map(|held| held.entries.clone())
            .unwrap_or_default()
    }
}

/// 把队列页那几行接上。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn bind(ui: &MainWindow, deck: &Deck) {
    let picking = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Viz>().on_queue_pick(move |entry| {
        let Some(ui) = weak.upgrade() else { return };
        pick(&ui, &picking, &entry);
    });

    // 封面与列表页同一条路:行滑进可见区时报一次,Rust 去取、去重、回填。
    let covering = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Viz>().on_queue_needs_cover(move |url| {
        let Some(ui) = weak.upgrade() else { return };
        covering.thumbnails.request(&ui, &url);
    });
}

/// 每秒那趟轮询叫一次。
///
/// **总数每一轮都报,行只在页开着时才建。** 两件事不能一起跳过:那颗
/// 「队列 · N」药丸是打开这一页的唯一入口,而它的显示条件就是这个总数
/// 大于零 —— 只在页开着时才算总数的话,总数恒为 0、药丸永远不出现、
/// 这一页于是永远打不开。2026-09-21 真机上就是这么撞见的。
///
/// 行则确实该省:五千首的模型每秒重建一次,而用户根本没在看。
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::music) fn refresh(
    ui: &MainWindow,
    deck: &Deck,
) {
    let open = ui.global::<Viz>().get_queue_page_open();
    if deck.remote.is_remote() {
        refresh_remote(ui, deck, open);
    } else {
        refresh_local(ui, deck, open);
    }
}

/// 本机:队列就在手上。
#[cfg(not(target_arch = "wasm32"))]
fn refresh_local(ui: &MainWindow, deck: &Deck, open: bool) {
    let queue = deck.queue.borrow();
    ui.global::<Viz>()
        .set_queue_total(queue.tracks().len() as i32);
    ui.global::<Viz>().set_queue_loading(false);
    if !open {
        return;
    }

    let current = queue.index();
    ui.global::<Viz>().set_queue_current(
        entry_id_at(deck, current).to_string().into(),
    );

    // 每秒一趟只更新上面那几个标量;行只在换了批、或同步上去换了条目号时重建
    let (queue_id, _, revision) = deck.execution.identity();
    let key = Shown::Local {
        batch: queue.batch(),
        queue_id,
        revision,
    };
    if !deck.queue_mirror.needs_rows(key) {
        fill_covers(ui, deck);
        return;
    }
    let rows: Vec<TrackRow> = queue
        .tracks()
        .iter()
        .enumerate()
        .map(|(at, track)| {
            row(deck, entry_id_at(deck, at), track)
        })
        .collect();
    push(ui, rows);
}

/// 遥控:按被控端报来的标识拉一份只读缓存。
#[cfg(not(target_arch = "wasm32"))]
fn refresh_remote(
    ui: &MainWindow,
    deck: &Deck,
    open: bool,
) {
    let (queue_id, revision, current, len) =
        deck.remote.with_view(|view, _| {
            (
                view.queue_id(),
                view.applied_revision(),
                view.entry_id(),
                view.queue_len(),
            )
        });

    ui.global::<Viz>().set_queue_total(len as i32);
    if !open {
        // 药丸只要那个总数。整份列表等用户真的打开这一页再去取 ——
        // 不然每换一版就白拉一趟几千条。
        return;
    }
    ui.global::<Viz>().set_queue_current(
        current
            .map(|entry| entry.to_string())
            .unwrap_or_default()
            .into(),
    );

    // 被控端那份还没同步到服务端(`docs/adr/0031` 八):没有可拉的东西,
    // 而那不是错误。列表空着,标题那行说「共 N 首」——用户至少知道有多少。
    let (Some(queue_id), Some(revision)) =
        (queue_id, revision)
    else {
        ui.global::<Viz>().set_queue_loading(false);
        if deck.queue_mirror.needs_rows(Shown::Unsynced) {
            push(ui, Vec::new());
        }
        return;
    };

    if deck.queue_mirror.holds(queue_id, revision) {
        ui.global::<Viz>().set_queue_loading(false);
        if !deck.queue_mirror.needs_rows(Shown::Remote {
            queue_id,
            revision,
        }) {
            fill_covers(ui, deck);
            return;
        }
        let rows: Vec<TrackRow> = deck
            .queue_mirror
            .rows()
            .iter()
            .map(|(entry, track)| row(deck, *entry, track))
            .collect();
        push(ui, rows);
        return;
    }

    // 还没有这一版:去取。取的这几秒标成「正在取」—— 与「队列是空的」
    // 分开说,两种长得一样的话用户会以为自己的歌没了。同一版已经在路上
    // 就等它,不再发第二次。
    ui.global::<Viz>().set_queue_loading(true);
    if !deck.queue_mirror.begin_fetch(queue_id, revision) {
        return;
    }
    let mirror = deck.queue_mirror.clone();
    let weak = ui.as_weak();
    let _ = slint::spawn_local(async move {
        let fetched =
            api::fetch_queue(queue_id, revision).await;
        let Some(ui) = weak.upgrade() else { return };
        match fetched {
            Ok(entries) => {
                mirror.put(
                    queue_id,
                    revision,
                    entries
                        .into_iter()
                        .map(|entry| {
                            (entry.entry_id, entry.track)
                        })
                        .collect(),
                );
            }
            Err(error) => {
                // 取不下来只是这一页画不出来,不影响那边继续放。
                mirror.fetch_failed(queue_id, revision);
                log::warn!("队列页取不到列表: {error}");
                ui.global::<Viz>().set_queue_loading(false);
            }
        }
    });
}

/// 点了某一条。
#[cfg(not(target_arch = "wasm32"))]
fn pick(ui: &MainWindow, deck: &Deck, entry: &str) {
    let Ok(entry_id) = entry.parse::<i64>() else {
        return;
    };

    if deck.remote.is_remote() {
        pick_remote(ui, deck, entry_id);
        return;
    }

    // 本机:跳过去就行,**不重建队列** —— `replace` 会把随机清掉再重洗,
    // 而用户点的是「放这一首」,不是「重洗一次」(见 `Queue::jump_to`)。
    let at = index_of(deck, entry_id);
    let Some(at) = at else { return };
    if deck.queue.borrow_mut().jump_to(at).is_some() {
        play_current(ui, deck);
        checkpoint(deck, at);
    }
}

/// 遥控:发一条带这个 `entry_id` 的意图,再叫被控端一声。
///
/// 队列本来就在服务端上,所以这一下不必重新上传任何曲目 —— 这正是
/// 「条目号从服务端读回来」那条决定在这里换来的便宜。
#[cfg(not(target_arch = "wasm32"))]
fn pick_remote(
    ui: &MainWindow,
    deck: &Deck,
    entry_id: i64,
) {
    let (Some(queue_id), Some(revision)) =
        deck.remote.with_view(|view, _| {
            (view.queue_id(), view.applied_revision())
        })
    else {
        crate::notice::show(
            ui,
            "那台设备的队列还没同步到服务端".to_owned(),
        );
        return;
    };

    let operation_id =
        crate::sync::link::fresh_operation_id();
    let deck = deck.clone();
    let weak = ui.as_weak();
    let _ = slint::spawn_local(async move {
        let wrote = api::set_queue_intent(
            queue_id,
            api::SetQueueIntentDto {
                device_id: deck
                    .remote
                    .target_id()
                    .unwrap_or_default(),
                revision,
                entry_id,
                operation_id: operation_id.clone(),
            },
        )
        .await;
        if let Err(error) = wrote {
            if let Some(ui) = weak.upgrade() {
                crate::notice::show(
                    &ui,
                    format!("没能切到那一首: {error}"),
                );
            }
            return;
        }
        deck.remote.send(app_core::RemoteCommand::Play {
            queue_id,
            revision,
            entry_id,
            operation_id,
        });
    });
}

/// 队列还没同步到服务端时,第 `at` 首用哪个号。
///
/// **必须与真号撞不上**:真号是 `BIGSERIAL`,永远为正,所以这里一律给负数。
/// 撞上的话点一行会跳到另一首 —— 而那种错只在「本机队列没同步上去、用户
/// 又打开了队列页」这个组合下出现,平时一次都不会撞见。
pub(in crate::music) const fn synthetic_entry_id(
    at: usize,
) -> i64 {
    -(at as i64) - 1
}

/// 这一批的第 `at` 首对应哪个条目号。
#[cfg(not(target_arch = "wasm32"))]
fn entry_id_at(deck: &Deck, at: usize) -> i64 {
    deck.execution
        .entry_at(at)
        .unwrap_or_else(|| synthetic_entry_id(at))
}

/// 反过来:某个条目号是这一批的第几首。
#[cfg(not(target_arch = "wasm32"))]
fn index_of(deck: &Deck, entry_id: i64) -> Option<usize> {
    let len = deck.queue.borrow().tracks().len();
    (0..len).find(|at| entry_id_at(deck, *at) == entry_id)
}

#[cfg(not(target_arch = "wasm32"))]
fn row(
    deck: &Deck,
    entry_id: i64,
    track: &TrackDto,
) -> TrackRow {
    TrackRow {
        // **装的是 `entry_id`**,不是曲目 id:同一首歌可以出现多次,
        // 按曲目认的话点第二次出现的那一条会放到第一条上去。
        id: entry_id.to_string().into(),
        title: track.title.clone().into(),
        artists: join_artists(&track.artists).into(),
        duration: format_duration(track.duration_ms).into(),
        loading: false,
        liked: false,
        cover_url: track
            .cover
            .clone()
            .unwrap_or_default()
            .into(),
        // 图**这里就查**,不等 `Thumbnails::apply` 回填:那一条只认列表页
        // 那个模型。没取过的仍然是空的,`needs-cover` 报上去,之后由
        // [`fill_covers`] 在每秒那一趟里补上。
        cover: track
            .cover
            .as_deref()
            .and_then(|url| deck.thumbnails.cached(url))
            .unwrap_or_default(),
    }
}

/// 行没重建时,把这一秒里新取到的缩略图补进还空着的那几行。
///
/// ponytail: 每秒扫一遍行(只读、不重建);几千行也是微秒级。真成了热点再按
/// 「还缺图的行」记一张表。
#[cfg(not(target_arch = "wasm32"))]
fn fill_covers(ui: &MainWindow, deck: &Deck) {
    use slint::Model as _;

    let model = ui.global::<Viz>().get_queue_rows();
    for index in 0..model.row_count() {
        let Some(mut row) = model.row_data(index) else {
            continue;
        };
        if row.cover.size().width > 0
            || row.cover_url.is_empty()
        {
            continue;
        }
        if let Some(cover) =
            deck.thumbnails.cached(&row.cover_url)
        {
            row.cover = cover;
            model.set_row_data(index, row);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn push(ui: &MainWindow, rows: Vec<TrackRow>) {
    // 与列表页同一条:模型整份换,不逐行改 —— 逐行改要先比对,
    // 而比对本身就要遍历一遍。
    let model = ui.global::<Viz>().get_queue_rows();
    if let Some(model) =
        model.as_any().downcast_ref::<VecModel<TrackRow>>()
    {
        model.set_vec(rows);
        return;
    }
    ui.global::<Viz>().set_queue_rows(
        Rc::new(VecModel::from(rows)).into(),
    );
}

#[cfg(test)]
mod tests;
