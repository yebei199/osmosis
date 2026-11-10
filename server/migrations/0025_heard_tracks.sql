-- 「听过」改成听够了才算(用户 2026-11-10 定):这一首放完或被切走时,累计听了
-- 时长的一半以上,或累计满 240 秒,先到哪条算哪条。出声那一刻不再算。
--
-- 听过是永久的,每个账号每首至多一行。判定发生在补记听了多久的那一刻
-- (`store::history::settle`),这里只存结论;起播事件仍在 play_events,
-- 最近播放、统计照旧从那里查询时聚合。
--
-- 回填:已补记时长的行按新口径判;没补记的(#157 之前的、组里放的、进程被杀的)
-- 算不出听了多久,沿用旧口径算听过 —— 不让列表里已经归进「已听过」的歌整批倒回来。

CREATE TABLE heard_tracks (
    account_id BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    platform TEXT NOT NULL,
    track_id TEXT NOT NULL,
    heard_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, platform, track_id)
);

-- 预取与未听池跨账号按曲目查
CREATE INDEX heard_tracks_track_idx ON heard_tracks (platform, track_id);

INSERT INTO heard_tracks (account_id, platform, track_id, heard_at)
SELECT account_id, platform, track_id, min(played_at)
FROM play_events
WHERE listened_ms IS NULL
   OR listened_ms >= 240000
   OR (duration_ms > 0 AND listened_ms * 2 >= duration_ms)
GROUP BY account_id, platform, track_id;
