-- 平台曲目的缓存。**不是镜像**(见 docs/adr/0018):写永远直发平台、冲突时
-- 平台赢、整张删掉只是慢一次而不丢东西。判定问题:删了会丢数据吗?会,那它
-- 已经不是缓存了。
--
-- 与 0002 的 local_playlist_tracks 各管各的。那张表存的是本地歌单的成员关系,
-- 真相在这边;这两张存的是平台那边的东西,只是留了一份省得每次重取。

CREATE TABLE platform_tracks (
    -- 曲目的身份是 (平台, 平台内 id),缺一不可 —— 见 bang-dream 的 docs/adr/0003
    platform TEXT NOT NULL,
    track_id TEXT NOT NULL,
    title TEXT NOT NULL,
    -- 别名/副标题,常见于日文原名。平台没给就是 NULL,不用空串冒充
    alias TEXT,
    -- 歌手名保持列表形态:怎么拼接是显示问题,属于 UI。
    -- 用数组而不是另开一张关联表 —— 这里没有「按歌手查」的需求,
    -- 有了再说,那时 unnest 也能建索引。
    artists TEXT[] NOT NULL,
    cover TEXT,
    duration_ms BIGINT NOT NULL,
    -- 这份详情是什么时候从平台拿到的。刷新策略读它,别处不读
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (platform, track_id)
);

-- 平台歌单的成员关系与顺序。
--
-- 按账号存:歌单归属是平台那边的事,而这边只认账号 —— 两个账号收藏了同一个
-- 歌单,各自刷新各自的那份,互不打架。
CREATE TABLE platform_playlist_tracks (
    account_id BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    -- 平台歌单 id。「我喜欢的」用下面那个保留值 —— 它在这张表里就是个普通歌单,
    -- 不单开一套表、也不单写一套代码。保留值取空串:平台的 id 恒非空,撞不上。
    playlist_id TEXT NOT NULL,
    platform TEXT NOT NULL,
    track_id TEXT NOT NULL,
    -- 歌单内的次序。平台给的顺序是有意义的,丢了用户的歌单就乱了
    position BIGINT NOT NULL,
    PRIMARY KEY (account_id, playlist_id, platform, track_id),
    -- 详情跟着成员关系走:曲目不在缓存里,这条成员关系就没有意义。
    -- 反过来不成立 —— 详情可以先于任何歌单存在(搜索结果也会填它)。
    FOREIGN KEY (platform, track_id)
        REFERENCES platform_tracks (platform, track_id) ON DELETE CASCADE
);

-- 读一个歌单就是按 position 扫这个前缀,建这条索引正是为它
CREATE INDEX platform_playlist_tracks_order_idx
    ON platform_playlist_tracks (account_id, playlist_id, position);
