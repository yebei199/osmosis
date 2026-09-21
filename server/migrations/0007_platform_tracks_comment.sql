-- 0004 的 SQL 注释里有一句与代码对不上的话:它说详情「搜索结果也会填它」。
-- 实际不会 —— `search_tracks` 与 `daily` 从不调 `put_details`,填这张表的只有三处,
-- 全在歌单那条路上:`store/cache.rs` 的 `set_playlist`、
-- `routes/catalog/catalog_cache.rs` 的 `cached_tracks` 与 `fill_details`。
--
-- 为什么不回头改 0004:见本目录 README.md 那条硬规则。
COMMENT ON TABLE platform_tracks IS
    '平台曲目详情缓存。只由歌单路径填:set_playlist / cached_tracks / fill_details。'
    '搜索与每日推荐不填它。成员关系表 platform_playlist_tracks 有外键指向这里,'
    '反向没有,所以详情行可以在歌单成员关系被删之后继续存在。';
