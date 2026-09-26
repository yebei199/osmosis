-- 每个账号最近一次取到的每日推荐(#147)。
--
-- 只为保留规则:在当天日推里的曲目,存进桶后不被清扫、在空间上限的取舍里排在
-- 「我的喜欢」之后、其他歌单之前。每次 /daily 取到新的一批就整批换掉,所以这里
-- 永远只有「最近一次」那一天。日推的真相仍在平台(docs/adr/0033)。

CREATE TABLE daily_picks (
    account_id BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    platform TEXT NOT NULL,
    track_id TEXT NOT NULL,
    PRIMARY KEY (account_id, platform, track_id)
);

-- 保留规则按曲目查「在不在任何人的日推里」
CREATE INDEX daily_picks_track_idx ON daily_picks (platform, track_id);
