-- 0007 那句「搜索与每日推荐不填它」自 #156 起不成立了:两条路由都把列表里的
-- 曲目写进来(routes/catalog/search.rs 的 remember_details)。改说明不改 0007,
-- 理由见本目录 README.md 那条硬规则。
COMMENT ON TABLE platform_tracks IS
    '平台曲目详情缓存。歌单路径(set_playlist / cached_tracks / fill_details)'
    '与日推、搜索(remember_details)都写它。成员关系表 platform_playlist_tracks'
    '有外键指向这里,反向没有,所以详情行可以在歌单成员关系被删之后继续存在。';
