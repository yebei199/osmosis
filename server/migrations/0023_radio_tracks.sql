-- 每个账号一份共享的电台歌单(#186):哪台设备加载的新歌都追加进来,所有设备读同一份。
--
-- 只记身份与加入次序;详情借 platform_tracks(加载时已写进缓存)。听没听过不存:
-- 查询时看 play_events,听过的照样留在这里,电台区的「已听过」要摆它们。

CREATE TABLE radio_tracks (
    id BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    platform TEXT NOT NULL,
    track_id TEXT NOT NULL,
    added_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, platform, track_id)
);
