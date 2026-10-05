-- #181 歌曲规则及歌手身份快照；旧数据为空数组，匹配时回退到名字。
ALTER TABLE block_rules DROP CONSTRAINT block_rules_kind_check;
ALTER TABLE block_rules ADD CONSTRAINT block_rules_kind_check
    CHECK (kind IN ('artist', 'tag', 'track', 'song', 'song_versions'));
ALTER TABLE platform_tracks ADD COLUMN artist_identities JSONB NOT NULL DEFAULT '[]';
ALTER TABLE play_queue_entries ADD COLUMN artist_identities JSONB NOT NULL DEFAULT '[]';
