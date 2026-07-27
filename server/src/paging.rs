//! 翻页:把一份全量标识列表切成一页。
//!
//! 这是 bang-dream 那条约定的落点 —— 它给的是**全量**标识列表(平台返回的曲目
//! 列表会被截断,标识列表不会),由调用方持有并自行切片,聚合层因此不必缓存任何东西。
//! 切完这一页再走 `GetTracks` 补全成完整曲目。
//!
//! 单独成模块只为一件事:切片是会 panic 的操作,而 offset/limit 来自
//! 查询串 —— 那是信任边界。这里把边界夹干净,上面的 handler 就不必再想。

/// 一页最多几条。上游的 `GetTracks` 是一次请求带全部 id,页太大容易被截断或超时。
pub const DEFAULT_PAGE_LIMIT: usize = 50;

/// 从全量标识列表里取一页。
///
/// `offset` 越界返回空页,不 panic;`limit` 为 0 用 [`DEFAULT_PAGE_LIMIT`]。
pub fn page(
    ids: &[String],
    offset: usize,
    limit: usize,
) -> &[String] {
    let limit = if limit == 0 {
        DEFAULT_PAGE_LIMIT
    } else {
        limit
    };
    // 两次 min 把两端都夹进合法范围,`start..end` 因此永远不会 panic。
    let start = offset.min(ids.len());
    let end = start.saturating_add(limit).min(ids.len());

    &ids[start..end]
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    fn ids(count: usize) -> Vec<String> {
        (0..count).map(|i| i.to_string()).collect()
    }

    /// 正常翻页:第二页从第 10 条开始,取 5 条。
    #[test]
    fn page_slices_ids_by_offset_and_limit() {
        let all = ids(30);

        assert_eq!(
            page(&all, 10, 5),
            ["10", "11", "12", "13", "14"]
        );
    }

    /// offset 超出总数返回空页。
    ///
    /// 直接 `&ids[offset..]` 会 panic —— 而 offset 来自查询串,
    /// 翻到最后一页再点一次「下一页」就能触发,这是必经之路不是恶意输入。
    #[test]
    fn page_beyond_end_is_empty_not_panic() {
        let all = ids(3);

        assert!(page(&all, 10, 5).is_empty());
    }

    /// 尾页只剩 2 条时,要 5 条也只给 2 条,不越界。
    #[test]
    fn page_clamps_limit_to_remaining() {
        let all = ids(12);

        assert_eq!(page(&all, 10, 5), ["10", "11"]);
    }

    /// `limit=0` 退回默认页大小。
    ///
    /// 老老实实返回空列表的话,界面上是「一首喜欢的都没有」——
    /// 一个不带 limit 的请求会被这样误读,而那正是最常见的调用方式。
    #[test]
    fn zero_limit_falls_back_to_default() {
        let all = ids(100);

        assert_eq!(
            page(&all, 0, 0).len(),
            DEFAULT_PAGE_LIMIT
        );
    }
}
