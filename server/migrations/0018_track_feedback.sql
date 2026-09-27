-- 每首歌的三态赞踩(#157)。独立于红心:红心管收藏,这张表管评价。
--
-- 一账号一曲目至多一行,取消就删行 —— 与「三态」直接对应,不必再判一个
-- 「未表态」的取值。

CREATE TABLE track_feedback (
    account_id BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    platform TEXT NOT NULL,
    track_id TEXT NOT NULL,
    verdict SMALLINT NOT NULL CHECK (verdict IN (1, -1)),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, platform, track_id)
);
