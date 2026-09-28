//! 歌单视图的分组条与筛选 chips(#160)。
//!
//! 分堆、筛选、chip 怎么数是 [`app_core::facets`] 的规则;这里只管状态放在哪、
//! 怎么投影成行、按下去做什么。
//!
//! 状态跟着**列表**走,不跟着视图走:分组方式换视图时留着(按歌手翻几个歌单是
//! 常见用法),选中的 chip 与折起来的堆换视图就清 —— 那是上一批歌的取值,
//! 带到这一批里要么筛空、要么对不上。

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use app_core::facets::{self, Chip, Chosen, Facet, Line};

use super::*;
use crate::{FacetChip, Shell};

/// 分组条此刻的状态。
#[derive(Default)]
pub(super) struct FacetState {
    /// 当前视图的整批,没筛过。`Deck::tracks` 是它筛过、排过之后的那一批。
    all: Vec<TrackDto>,
    /// 当前视图是歌单类(见 [`supports`])。不是的话照原样摆,忽略下面三样。
    enabled: bool,
    grouping: Option<Facet>,
    chosen: Chosen,
    /// 按摆出来的次序;`toggle-chip` 的下标指它。
    chips: Vec<Chip>,
    collapsed: HashSet<String>,
    /// 上一次推上去几行(堆头也算)。只改加载态时拿它判断模型还是不是这一份。
    rows: Cell<usize>,
}

pub(super) type Facets = Rc<RefCell<FacetState>>;

/// 哪些视图算歌单:日推、最近、歌单、歌手。搜索结果与电台不是。
pub(super) fn supports(source: &ViewSource) -> bool {
    matches!(
        source,
        ViewSource::Daily
            | ViewSource::Recent
            | ViewSource::Playlist(..)
            | ViewSource::Artist(_)
    )
}

impl FacetState {
    /// 换了视图:选中的 chip 与折叠是上一批歌的,一并清掉。分组方式留着。
    pub(super) fn leave_view(&mut self) {
        self.chosen.clear();
        self.collapsed.clear();
    }

    /// 换了一批歌(同一视图刷新,或刚切过来)。
    pub(super) fn load(
        &mut self,
        tracks: Vec<TrackDto>,
        enabled: bool,
    ) {
        self.all = tracks;
        self.enabled = enabled;
        self.chips = facets::chips(&self.all);
        // 同一视图刷新后某个取值没了(比如刚取消了最后一个赞),它的 chip
        // 不在了,选中它只会筛空
        let offered: HashSet<(Facet, &str)> = self
            .chips
            .iter()
            .map(|chip| (chip.facet, chip.label.as_str()))
            .collect();
        self.chosen.retain(|(facet, label)| {
            offered.contains(&(*facet, label.as_str()))
        });
    }

    fn arranged(&self) -> Vec<Line> {
        if self.enabled {
            facets::arrange(
                &self.all,
                &self.chosen,
                self.grouping,
                &self.collapsed,
            )
        } else {
            (0..self.all.len()).map(Line::Track).collect()
        }
    }

    /// 点一首歌时排进队列的那一批。
    pub(super) fn batch(&self) -> Vec<TrackDto> {
        let order = if self.enabled {
            facets::batch(
                &self.all,
                &self.chosen,
                self.grouping,
            )
        } else {
            (0..self.all.len()).collect()
        };
        order
            .into_iter()
            .map(|index| self.all[index].clone())
            .collect()
    }

    /// 分出了几堆;不分组是 0。
    fn pile_count(&self) -> usize {
        self.arranged()
            .iter()
            .filter(|line| {
                matches!(line, Line::Header { .. })
            })
            .count()
    }

    /// 列表的行:堆头与歌。
    pub(super) fn rows(
        &self,
        loading: Option<&str>,
    ) -> Vec<TrackRow> {
        let rows: Vec<TrackRow> = self
            .arranged()
            .into_iter()
            .map(|line| match line {
                Line::Track(index) => to_rows(
                    std::slice::from_ref(&self.all[index]),
                    loading,
                )
                .remove(0),
                Line::Header {
                    label,
                    count,
                    collapsed,
                } => header_row(label, count, collapsed),
            })
            .collect();
        self.rows.set(rows.len());
        rows
    }

    /// 上一次推上去几行。
    pub(super) fn row_count(&self) -> usize {
        self.rows.get()
    }

    /// 投影分组条:哪一项选中、有哪些 chip、选了几个、这个视图挂不挂条。
    fn project(&self, ui: &MainWindow) {
        let player = ui.global::<Player>();
        player.set_facets_enabled(
            self.enabled && !self.all.is_empty(),
        );
        player.set_grouping(
            self.grouping.map_or(0, Facet::index),
        );
        let chips: Vec<FacetChip> = self
            .chips
            .iter()
            .map(|chip| FacetChip {
                text: format!(
                    "{} {}",
                    chip.label, chip.count
                )
                .into(),
                chosen: self.chosen.contains(&(
                    chip.facet,
                    chip.label.clone(),
                )),
            })
            .collect();
        player
            .set_chips(ModelRc::new(VecModel::from(chips)));
        player.set_pile_count(
            i32::try_from(self.pile_count())
                .unwrap_or(i32::MAX),
        );
        player.set_chosen_count(
            i32::try_from(self.chosen.len())
                .unwrap_or(i32::MAX),
        );
    }
}

/// 堆头那一行:没有 id(点播认不出它),时长那一格写这一堆几首。
fn header_row(
    label: String,
    count: usize,
    collapsed: bool,
) -> TrackRow {
    TrackRow {
        title: label.into(),
        duration: format!("{count} 首").into(),
        header: true,
        collapsed,
        ..Default::default()
    }
}

/// 按当前状态重排一遍:队列那一批、列表的行、分组条。
pub(super) fn relayout(ui: &MainWindow, deck: &Deck) {
    let batch = deck.facets.borrow().batch();
    *deck.tracks.borrow_mut() = batch;
    let loading =
        loading_id(deck.playback.borrow().state())
            .map(str::to_owned);
    push_rows(ui, deck, loading.as_deref());
    deck.facets.borrow().project(ui);
}

/// 接上分组条的三个回调。
pub(super) fn bind(ui: &MainWindow, deck: &Deck) {
    let labels: Vec<slint::SharedString> =
        std::iter::once("不分组")
            .chain(
                Facet::ALL
                    .iter()
                    .map(|facet| facet.label()),
            )
            .map(Into::into)
            .collect();
    ui.global::<Player>().set_grouping_labels(
        ModelRc::new(VecModel::from(labels)),
    );

    let grouped = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_set_grouping(move |index| {
        let Some(ui) = weak.upgrade() else { return };
        let grouping = Facet::from_index(index);
        {
            let mut state = grouped.facets.borrow_mut();
            state.grouping = grouping;
            state.collapsed.clear();
        }
        // 卡墙只筛选、不分组(#160,用户 2026-09-27 定):一选分组就切回列表
        if grouping.is_some() {
            ui.global::<Shell>()
                .invoke_set_view_wall(false);
        }
        relayout(&ui, &grouped);
    });

    let chipped = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_toggle_chip(move |index| {
        let Some(ui) = weak.upgrade() else { return };
        {
            let mut state = chipped.facets.borrow_mut();
            let Some(chip) = usize::try_from(index)
                .ok()
                .and_then(|index| state.chips.get(index))
            else {
                return;
            };
            let key = (chip.facet, chip.label.clone());
            if !state.chosen.remove(&key) {
                state.chosen.insert(key);
            }
        }
        relayout(&ui, &chipped);
    });

    let folded = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Player>().on_toggle_pile(move |label| {
        let Some(ui) = weak.upgrade() else { return };
        {
            let mut state = folded.facets.borrow_mut();
            let label = label.to_string();
            if !state.collapsed.remove(&label) {
                state.collapsed.insert(label);
            }
        }
        relayout(&ui, &folded);
    });
}
