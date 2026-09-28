-- 屏蔽规则(#161)。屏蔽是规则不是评价:按歌手 / 标签 / 单曲建,命中的歌
-- 在所有列表里隐藏,队列与电台跳过。踩只是评价,在 track_feedback。

CREATE TABLE block_rules (
    id BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('artist', 'tag', 'track')),
    -- 歌手名、标签名,或平台内曲目 id
    value TEXT NOT NULL,
    -- 设置页「已屏蔽」显示的那一行;单曲靠它说出是哪首歌
    label TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- 同一条规则只有一行,重复屏蔽天然幂等
    UNIQUE (account_id, kind, value)
);
