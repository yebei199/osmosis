-- 本地歌单。真相在这里,平台不知道它们存在(见 docs/adr/0016)。

CREATE TABLE local_playlists (
    id BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX local_playlists_account_id_idx ON local_playlists (account_id);

CREATE TABLE local_playlist_tracks (
    playlist_id BIGINT NOT NULL REFERENCES local_playlists (id) ON DELETE CASCADE,
    -- 曲目的身份是 (平台, 平台内 id),缺一不可 —— 见 bang-dream 的 docs/adr/0003。
    -- 只存标识不存元数据:曲目详情的真相在平台,镜像下来就要负责它的时效。
    platform TEXT NOT NULL,
    track_id TEXT NOT NULL,
    -- 加入顺序。用递增的位置而不是时间戳:同一批加入的几首要能稳定排序。
    position BIGINT NOT NULL,
    -- 同一首歌在一个歌单里只有一条,重复加入因此天然幂等
    PRIMARY KEY (playlist_id, platform, track_id)
);

CREATE INDEX local_playlist_tracks_order_idx
    ON local_playlist_tracks (playlist_id, position);
