//! 已下载音乐的筛选、选择和系统删除授权状态。

use super::download::{
    DownloadDeletion, DownloadEntry, DownloadListing,
    DownloadStore,
};
use std::collections::HashSet;

/// 音乐管理状态只存本地条目，不修改播放队列。
#[derive(Default)]
pub(super) struct State {
    /// 最近一次平台目录快照。
    pub entries: Vec<DownloadEntry>,
    /// 按平台身份保存选择，不使用过滤后的行下标。
    pub selected: HashSet<String>,
    /// 当前本地子串搜索。
    pub keyword: String,
    /// 正在等待系统删除确认。
    pub busy: bool,
    /// 读取权限限制或目录错误。
    pub note: String,
    /// 最近删除结果，供提示投影。
    pub result: Option<DownloadDeletion>,
}

impl State {
    /// 换平台快照，移除已经不存在的选择。
    pub fn load(&mut self, listing: DownloadListing) {
        self.entries = listing.entries;
        self.note = listing.note;
        self.selected.retain(|id| {
            self.entries.iter().any(|item| &item.id == id)
        });
    }

    /// 所有搜索字段使用同一份大小写归一化关键词。
    pub fn visible(&self) -> Vec<&DownloadEntry> {
        let keyword = self.keyword.trim().to_lowercase();
        self.entries
            .iter()
            .filter(|entry| {
                [
                    entry.title.as_str(),
                    entry.artist.as_str(),
                    entry.file_name.as_str(),
                ]
                .iter()
                .any(|field| {
                    field.to_lowercase().contains(&keyword)
                })
            })
            .collect()
    }

    /// 总占用始终以整份目录为准。
    pub fn total(&self) -> u64 {
        self.entries.iter().fold(0u64, |size, entry| {
            size.saturating_add(entry.size)
        })
    }

    /// 切换一个存在条目的选中状态，授权期间保持原选择。
    pub fn toggle(&mut self, id: &str) {
        if self.busy
            || !self
                .entries
                .iter()
                .any(|entry| entry.id == id)
        {
            return;
        }
        if !self.selected.remove(id) {
            self.selected.insert(id.to_owned());
        }
    }

    /// 平台 pending 不计成功，空选择和重复确认都不调删除。
    pub fn delete(
        &mut self,
        store: &dyn DownloadStore,
    ) -> std::io::Result<()> {
        if self.busy || self.selected.is_empty() {
            return Ok(());
        }
        self.result = None;
        let ids = self
            .entries
            .iter()
            .filter(|entry| {
                self.selected.contains(&entry.id)
            })
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>();
        let result = store.delete(&ids)?;
        if result.pending {
            self.busy = true;
            return Ok(());
        }
        self.finish(store, result)
    }

    /// 系统确认结束后刷新目录，刷新失败也保留已经得到的删除结果。
    pub fn finish(
        &mut self,
        store: &dyn DownloadStore,
        result: DownloadDeletion,
    ) -> std::io::Result<()> {
        self.busy = false;
        self.result = Some(result);
        self.load(store.list()?);
        Ok(())
    }
}

/// 模型和统计使用同一份状态，筛选不改变整份目录占用。
fn project(ui: &crate::MainWindow, state: &State) {
    use slint::ComponentHandle as _;
    let global = ui.global::<crate::Downloads>();
    let rows = state
        .visible()
        .into_iter()
        .map(|entry| crate::DownloadRow {
            id: entry.id.clone().into(),
            file_name: entry.file_name.clone().into(),
            title: entry.title.clone().into(),
            artist: entry.artist.clone().into(),
            size: format!(
                "{:.2} MB",
                entry.size as f64 / (1024.0 * 1024.0)
            )
            .into(),
            selected: state.selected.contains(&entry.id),
        })
        .collect::<Vec<_>>();
    global.set_rows(slint::ModelRc::new(
        slint::VecModel::from(rows),
    ));
    global.set_total(
        format!(
            "共 {} 首 · 占用 {:.2} MB",
            state.entries.len(),
            state.total() as f64 / (1024.0 * 1024.0)
        )
        .into(),
    );
    global.set_note(state.note.clone().into());
    global.set_selected_count(
        i32::try_from(state.selected.len())
            .unwrap_or(i32::MAX),
    );
    global.set_busy(state.busy);
}

/// 提示只消费一次平台最终结果，取消和部分失败各自说明。
fn announce(ui: &crate::MainWindow, state: &mut State) {
    let Some(result) = state.result.take() else {
        return;
    };
    let text =
        if result.cancelled && result.deleted.is_empty() {
            "未删除，已取消系统确认".to_owned()
        } else {
            let bytes = result.deleted.iter().fold(
                0u64,
                |total, entry| {
                    total.saturating_add(entry.size)
                },
            );
            let mut text = format!(
                "已删除 {} 首,释放 {:.2} MB",
                result.deleted.len(),
                bytes as f64 / (1024.0 * 1024.0)
            );
            if !result.failures.is_empty() {
                text.push_str(&format!(
                    "；{}",
                    result.failures.join("；")
                ));
            }
            text
        };
    crate::notice::show(ui, text);
}

/// 目录操作失败保持可恢复入口，并把原因给用户。
fn refresh(
    ui: &crate::MainWindow,
    state: &mut State,
    store: &dyn DownloadStore,
) -> bool {
    let pending = match store.list() {
        Ok(listing) => {
            let pending = listing.pending;
            state.load(listing);
            pending
        }
        Err(err) => {
            state.note =
                format!("无法读取已下载歌曲: {err}");
            false
        }
    };
    project(ui, state);
    pending
}

/// 接独立音乐 global；计时器只在系统权限或删除确认未回传时运行。
pub(super) fn bind(ui: &crate::MainWindow) {
    use slint::ComponentHandle as _;
    use std::cell::RefCell;
    use std::rc::Rc;
    let Some(store) = super::download::STORE.get() else {
        return;
    };
    let store = store.as_ref();
    let state = Rc::new(RefCell::new(State::default()));
    let timer = Rc::new(slint::Timer::default());

    let polling = state.clone();
    let timer_weak = Rc::downgrade(&timer);
    let weak = ui.as_weak();
    let poll = move || {
        let Some(ui) = weak.upgrade() else { return };
        let mut state = polling.borrow_mut();
        if state.busy {
            match store.poll_delete() {
                Ok(Some(result)) => {
                    if let Err(err) =
                        state.finish(store, result)
                    {
                        state.note =
                            format!("刷新失败: {err}");
                    }
                    announce(&ui, &mut state);
                }
                Ok(None) => {}
                Err(err) => {
                    state.busy = false;
                    state.note =
                        format!("系统删除失败: {err}");
                    crate::notice::show(
                        &ui,
                        state.note.clone(),
                    );
                }
            }
        }
        let pending = refresh(&ui, &mut state, store);
        if !pending
            && !state.busy
            && let Some(timer) = timer_weak.upgrade()
        {
            timer.stop();
        }
    };
    // 回调共享但不捕获计时器强引用，窗口退出后不会形成循环。
    let poll: Rc<dyn Fn()> = Rc::new(poll);

    let showing = state.clone();
    let showing_timer = timer.clone();
    let showing_poll = poll.clone();
    let weak = ui.as_weak();
    ui.global::<crate::Downloads>().on_show(move || {
        let Some(ui) = weak.upgrade() else { return };
        if let Err(err) = store.request_access() {
            crate::notice::show(
                &ui,
                format!("读取音乐授权失败: {err}"),
            );
        }
        let pending =
            refresh(&ui, &mut showing.borrow_mut(), store);
        if pending {
            let poll = showing_poll.clone();
            showing_timer.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(250),
                move || poll(),
            );
        }
    });

    let refreshing = state.clone();
    let weak = ui.as_weak();
    ui.global::<crate::Downloads>().on_refresh(move || {
        if let Some(ui) = weak.upgrade() {
            refresh(
                &ui,
                &mut refreshing.borrow_mut(),
                store,
            );
        }
    });

    let searching = state.clone();
    let weak = ui.as_weak();
    ui.global::<crate::Downloads>().on_search(
        move |keyword| {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = searching.borrow_mut();
            state.keyword = keyword.to_string();
            project(&ui, &state);
        },
    );

    let selecting = state.clone();
    let weak = ui.as_weak();
    ui.global::<crate::Downloads>().on_toggle(move |id| {
        let Some(ui) = weak.upgrade() else { return };
        let mut state = selecting.borrow_mut();
        state.toggle(id.as_str());
        project(&ui, &state);
    });

    let deleting_timer = timer;
    let weak = ui.as_weak();
    ui.global::<crate::Downloads>().on_delete_selected(
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut state = state.borrow_mut();
            if let Err(err) = state.delete(store) {
                crate::notice::show(
                    &ui,
                    format!("删除失败: {err}"),
                );
            }
            announce(&ui, &mut state);
            project(&ui, &state);
            if state.busy {
                let poll = poll.clone();
                deleting_timer.start(
                    slint::TimerMode::Repeated,
                    std::time::Duration::from_millis(250),
                    move || poll(),
                );
            }
        },
    );
}
