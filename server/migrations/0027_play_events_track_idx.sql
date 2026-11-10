-- 按账号、按曲目查起播事件:电台去重(`store::history::known_among`)逐首问 EXISTS,
-- 最近播放(`store::history::recent`)按 (platform, track_id, id DESC) 去重。原来只有
-- 按时间排的索引,两处都要扫这个账号的全部事件。
CREATE INDEX play_events_track_idx
    ON play_events (account_id, platform, track_id, id);
