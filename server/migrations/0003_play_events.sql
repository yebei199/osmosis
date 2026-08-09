-- 播放事件。起播即追加一条,只增不改(见 docs/adr/0016)。
--
-- 不记"听了多久":补记要靠客户端在退出/切歌时再发一次,而崩溃与断网时那一条就丢了。
-- 口径将来要改(比如只算听完的)时,原始事件都在,改的是查询而不是数据。

CREATE TABLE play_events (
    id BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    -- 曲目身份是 (平台, 平台内 id),与本地歌单同一个理由
    platform TEXT NOT NULL,
    track_id TEXT NOT NULL,
    played_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 「最近播放」按账号取最新的几条,这个索引就是为它建的
CREATE INDEX play_events_recent_idx
    ON play_events (account_id, played_at DESC, id DESC);
