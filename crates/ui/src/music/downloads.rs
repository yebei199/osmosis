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
