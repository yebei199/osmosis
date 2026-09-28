-- 账号级自定义标签(#158)。同一首歌在哪个歌单里标签都一样。
-- 期 2 的音频模型也会打标签(#162),落进同一套表,靠 source 区分。

CREATE TABLE tags (
    id BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT 'manual',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- 同一个账号下标签名唯一,建重名标签时直接复用已有的那个
    UNIQUE (account_id, name)
);

CREATE INDEX tags_account_id_idx ON tags (account_id);

CREATE TABLE track_tags (
    account_id BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    -- 曲目的身份是 (平台, 平台内 id),缺一不可 —— 见 bang-dream 的 docs/adr/0003
    platform TEXT NOT NULL,
    track_id TEXT NOT NULL,
    tag_id BIGINT NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    -- 同一首歌同一个标签只有一条,重复打天然幂等
    PRIMARY KEY (account_id, platform, track_id, tag_id)
);

-- 删标签时按 tag_id 找出所有关联(ON DELETE CASCADE 用到)
CREATE INDEX track_tags_tag_idx ON track_tags (tag_id);
-- 查一首歌打了哪些标签
CREATE INDEX track_tags_track_idx ON track_tags (account_id, platform, track_id);
