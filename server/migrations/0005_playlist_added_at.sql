-- 歌单成员关系带上「加入时间」,展示次序改由它决定(见 docs/adr/0021)。
--
-- 起因:红心的曲目标识来自网易云的 /api/song/like/get,那个接口返回裸数字数组,
-- 顺序稳定却不表示任何东西 —— 976 首里当天新点的排第 120 位。而歌单详情的
-- trackIds 每条都带 at,毫秒级、逐条唯一,平台自己就按它倒序发。

ALTER TABLE platform_playlist_tracks
    ADD COLUMN added_at TIMESTAMPTZ;

-- 可空,且**不回填**。0005 之前写下的行没有这个时刻,平台那边也无从追溯;
-- 编一个出来会让它们混进真实时间里排序,而错的顺序不会有人报错。
-- 它们按 NULLS LAST 退回原来的 position 次序,下一次刷新自然带上真时间。
COMMENT ON COLUMN platform_playlist_tracks.added_at IS
    '这首歌被加进这个歌单的时刻,平台给的。属于成员关系而非曲目:同一首歌在两个歌单里各有各的加入时刻。';

-- 读一个歌单就是按这个前缀扫。DESC 与查询里的 ORDER BY 对齐 ——
-- 方向不一致的话 Postgres 仍能倒着走索引,但 NULLS LAST 那一段会退化。
CREATE INDEX platform_playlist_tracks_added_at_idx
    ON platform_playlist_tracks (account_id, playlist_id, added_at DESC NULLS LAST, position);
