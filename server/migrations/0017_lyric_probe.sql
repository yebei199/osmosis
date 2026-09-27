-- 歌词标记与它的探测队列(#156)。歌单分类视图要按「有没有歌词」分组。
--
-- 队列就挂在 platform_tracks 这一行上,不另开一张表:每一行都要探一次,
-- lyric_kind 还是 unknown 就等于「在排队」,入队这件事因此不存在 —— 任何
-- 写进这张表的路径(歌单、日推、搜索)自动排上。形状照 0013 的 prefetch_jobs:
-- worker 用 FOR UPDATE SKIP LOCKED 领,领走时把 lyric_probe_after 推到租约到期,
-- 失败按次数退避,领满上限就不再领(留在 unknown,等人看日志)。
--
-- 详情刷新(put_details 的覆盖)不碰这三列:歌词有没有,不随歌名改动而变。

ALTER TABLE platform_tracks
    -- unknown:还没探;none:平台没有歌词;instrumental:平台标了纯音乐;
    -- lyric:有原文歌词;translated:还带翻译
    ADD COLUMN lyric_kind TEXT NOT NULL DEFAULT 'unknown'
        CHECK (lyric_kind IN ('unknown', 'none', 'instrumental', 'lyric', 'translated')),
    ADD COLUMN lyric_attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN lyric_probe_after TIMESTAMPTZ NOT NULL DEFAULT now();

-- worker 按它找「到点了、还没探的」
CREATE INDEX platform_tracks_lyric_due_idx
    ON platform_tracks (lyric_probe_after) WHERE lyric_kind = 'unknown';
