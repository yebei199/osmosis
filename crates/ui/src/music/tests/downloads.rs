//! 本地管理规则的确定性检查。平台权限返回是唯一替身。

use super::super::download::*;
use super::super::downloads::State;
use similar_asserts::assert_eq;
use slint::ComponentHandle as _;
use std::cell::Cell;
use std::sync::Mutex;

/// 三个字段各有独特子串，便于判断匹配来源。
fn entry(id: &str, size: u64) -> DownloadEntry {
    DownloadEntry {
        id: id.into(),
        file_name: format!("File-{id}.mp3"),
        title: format!("Title-{id}"),
        artist: format!("Artist-{id}"),
        size,
        modified: 42,
    }
}

/// 保留真实管理规则，替换系统权限与文件删除返回边界。
struct Store {
    /// 可观察的目录快照。
    entries: Mutex<Vec<DownloadEntry>>,
    /// 下一次系统删除结果。
    outcome: Mutex<DownloadDeletion>,
    /// 去重检查的边界调用次数。
    calls: Mutex<usize>,
}

impl Store {
    /// 每条用例拥有自己的条目和系统返回。
    fn new(outcome: DownloadDeletion) -> Self {
        Self {
            entries: Mutex::new(vec![
                entry("a", 10),
                entry("b", 20),
            ]),
            outcome: Mutex::new(outcome),
            calls: Mutex::new(0),
        }
    }
}

impl DownloadStore for Store {
    /// 本组不替换下载写入链路。
    fn open(
        &self,
        _: &str,
    ) -> std::io::Result<(
        Box<dyn std::io::Write + Send>,
        Box<dyn DownloadCommit>,
    )> {
        Err(std::io::Error::other("not a download writer"))
    }
    /// 仅用于平台提示。
    fn location(&self) -> String {
        "test music".into()
    }
    /// 独有快照允许管理规则重新查询。
    fn list(&self) -> std::io::Result<DownloadListing> {
        Ok(DownloadListing {
            entries: self
                .entries
                .lock()
                .expect("entries lock")
                .clone(),
            note: String::new(),
            pending: false,
        })
    }
    /// 只在系统明确返回删除成功时移除对应边界条目。
    fn delete(
        &self,
        ids: &[String],
    ) -> std::io::Result<DownloadDeletion> {
        *self.calls.lock().expect("calls lock") += 1;
        let outcome = self
            .outcome
            .lock()
            .expect("outcome lock")
            .clone();
        assert!(
            outcome
                .deleted
                .iter()
                .all(|item| ids.contains(&item.id))
        );
        let mut entries =
            self.entries.lock().expect("entries lock");
        entries.retain(|item| {
            !outcome
                .deleted
                .iter()
                .any(|deleted| deleted.id == item.id)
        });
        Ok(outcome)
    }
}

/// 歌名、歌手、文件名均做大小写不敏感子串；空词保持全量。
#[test]
fn search_matches_title_artist_and_filename_without_case() {
    let mut state = State::default();
    state.load(DownloadListing {
        entries: vec![
            entry("a", 10),
            entry("b", 20),
            DownloadEntry {
                title: "紅蓮華".into(),
                ..entry("c", 30)
            },
        ],
        ..Default::default()
    });
    for keyword in ["TITLE-A", "artist-A", "FILE-A"] {
        state.keyword = keyword.into();
        assert_eq!(
            state
                .visible()
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["a"]
        );
    }
    state.keyword = "蓮".into();
    assert_eq!(state.visible()[0].id, "c");
    state.keyword.clear();
    assert_eq!(state.visible().len(), 3);
    state.keyword = "absent".into();
    assert!(state.visible().is_empty());
}

/// 筛选只改变显示结果，不能把总占用缩成筛选后的大小。
#[test]
fn total_space_uses_all_entries_not_filtered_rows() {
    let mut state = State::default();
    state.load(DownloadListing {
        entries: vec![entry("a", 10), entry("b", 20)],
        ..Default::default()
    });
    state.keyword = "Title-a".into();
    assert_eq!(state.visible().len(), 1);
    assert_eq!(state.total(), 30);
}

/// 搜索变化与目录顺序变化都不把已选身份变成另一个文件。
#[test]
fn selection_survives_filter_by_id() {
    let store = Store::new(DownloadDeletion {
        deleted: vec![entry("a", 10)],
        ..Default::default()
    });
    let mut state = State::default();
    state.load(store.list().expect("listing"));
    state.toggle("a");
    state.keyword = "Title-b".into();
    assert_eq!(state.visible()[0].id, "b");
    state.load(DownloadListing {
        entries: vec![entry("b", 20), entry("a", 10)],
        ..Default::default()
    });
    state.delete(&store).expect("delete selected");
    assert_eq!(
        store.list().expect("remaining").entries,
        [entry("b", 20)]
    );
}

/// 系统拒绝其中一条时，统计只包含真删除的条目。
#[test]
fn partial_delete_reports_only_removed_entries() {
    let store = Store::new(DownloadDeletion {
        deleted: vec![entry("a", 10)],
        failures: vec!["b: denied".into()],
        ..Default::default()
    });
    let mut state = State::default();
    state.load(store.list().expect("listing"));
    state.toggle("a");
    state.toggle("b");
    state.delete(&store).expect("partial outcome");
    assert_eq!(state.entries, [entry("b", 20)]);
    let result = state.result.as_ref().expect("result");
    assert_eq!(
        result
            .deleted
            .iter()
            .map(|item| item.size)
            .sum::<u64>(),
        10
    );
    assert_eq!(result.failures, ["b: denied"]);
}

/// pending 返回不算成功，连点也只能发起一次系统操作。
#[test]
fn pending_authorization_disables_duplicate_delete() {
    let store = Store::new(DownloadDeletion {
        pending: true,
        ..Default::default()
    });
    let mut state = State::default();
    state.load(store.list().expect("listing"));
    state.toggle("a");
    state.delete(&store).expect("request");
    state.delete(&store).expect("duplicate ignored");
    assert!(state.busy);
    assert_eq!(*store.calls.lock().expect("calls"), 1);
    assert!(state.result.is_none());
    state
        .finish(
            &store,
            DownloadDeletion {
                cancelled: true,
                ..Default::default()
            },
        )
        .expect("cancel");
    assert!(!state.busy);
}

/// 取消保持文件快照，并明确零条删除，不伪造释放空间。
#[test]
fn cancelled_delete_preserves_files_and_reports_zero() {
    let store = Store::new(DownloadDeletion {
        cancelled: true,
        ..Default::default()
    });
    let mut state = State::default();
    state.load(store.list().expect("listing"));
    state.toggle("a");
    state.toggle("b");
    state.delete(&store).expect("cancelled result");
    assert_eq!(
        state.entries,
        [entry("a", 10), entry("b", 20)]
    );
    let result =
        state.result.as_ref().expect("cancel result");
    assert!(result.cancelled);
    assert!(result.deleted.is_empty());
    assert_eq!(state.total(), 30);
}

/// 读取权限被拒时，自有条目与原因同时留下，仍可选择。
#[test]
fn declined_read_permission_keeps_owned_downloads() {
    let mut state = State::default();
    state.load(DownloadListing {
        entries: vec![entry("a", 10)],
        note: "未授权读取音乐，仅显示本次安装下载的歌曲"
            .into(),
        pending: false,
    });
    assert_eq!(state.visible().len(), 1);
    state.toggle("a");
    assert!(state.selected.contains("a"));
    assert!(state.note.contains("未授权"));
}

/// 管理状态独立于播放和浏览上下文，取消操作不能清掉曲目。
#[test]
fn leaving_downloads_preserves_playback_and_browse_state() {
    let (ui, deck) = super::super::fixtures::deck_window();
    let before = deck.tracks.borrow().clone();
    let marker = Cell::new(0);
    let store = Store::new(DownloadDeletion::default());
    let mut state = State::default();
    state.load(store.list().expect("listing"));
    state.toggle("a");
    state.keyword = "Title-a".into();
    marker.set(state.visible().len());
    assert_eq!(marker.get(), 1);
    assert_eq!(*deck.tracks.borrow(), before);
    assert_eq!(
        ui.global::<crate::Shell>().get_music_section(),
        0
    );
}
