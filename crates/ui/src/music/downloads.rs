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
    pub fn load(&mut self, _listing: DownloadListing) {}
    /// 过滤后的条目引用。
    pub fn visible(&self) -> Vec<&DownloadEntry> {
        Vec::new()
    }
    /// 总占用始终以整份目录为准。
    pub fn total(&self) -> u64 {
        0
    }
    /// 切换一个存在条目的选中状态。
    pub fn toggle(&mut self, _id: &str) {}
    /// 仅把选中身份交给落点，并拦截授权期间的重复提交。
    pub fn delete(
        &mut self,
        _store: &dyn DownloadStore,
    ) -> std::io::Result<()> {
        Ok(())
    }
    /// 系统确认结束后刷新真实目录。
    pub fn finish(
        &mut self,
        _store: &dyn DownloadStore,
        _result: DownloadDeletion,
    ) -> std::io::Result<()> {
        Ok(())
    }
}
