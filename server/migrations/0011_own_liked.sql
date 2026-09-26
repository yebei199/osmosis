-- 「我的喜欢」归自家库(#146,docs/adr/0033):它是一个系统自带的本地歌单,
-- 网易云的红心只是一次性导入的来源。
--
-- 系统歌单用 system 列认,普通本地歌单这一列是 NULL。每个账号每种系统歌单
-- 至多一个,由下面那条部分唯一索引钉住 —— 并发的两次「没有就建」撞在这里,
-- 而不是各建一个。

ALTER TABLE local_playlists ADD COLUMN system TEXT;

CREATE UNIQUE INDEX local_playlists_system_idx
    ON local_playlists (account_id, system)
    WHERE system IS NOT NULL;

-- 成员关系带上加入时刻:导入时存网易云给的 at,之后点心时是此刻。
-- 已有的行留 NULL,不回填 —— 与 0005 同一个理由,编出来的时刻会混进真实时间里排序。
ALTER TABLE local_playlist_tracks ADD COLUMN added_at TIMESTAMPTZ;
ALTER TABLE local_playlist_tracks ALTER COLUMN added_at SET DEFAULT now();
