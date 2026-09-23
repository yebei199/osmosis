-- 存进对象存储(RustFS)的曲目音频,一行一个对象(#126)。
--
-- 这是缓存:删了只是下一次重新找网易云要,与 0004 同一个判定。对象本身在桶里,
-- 这里记的是「桶里有什么、什么时候该删」—— 清理任务只读这张表,不列桶。

CREATE TABLE stored_tracks (
    platform TEXT NOT NULL,
    track_id TEXT NOT NULL,
    -- 取源档位,如 'high'。同一首换了档位是另一个对象
    quality TEXT NOT NULL,
    object_key TEXT NOT NULL,
    -- 上游给的容器格式,原样存、原样交回客户端,不转码
    format TEXT NOT NULL,
    bit_rate INTEGER NOT NULL,
    -- 对象的字节数。桶的用量应当等于这一列之和
    bytes BIGINT NOT NULL,
    stored_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- 最后一次被播放(或被取消红心)的时刻。没人红心的,离它满三天就删
    last_played_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (platform, track_id, quality)
);

-- 清理任务按它扫「最后一次播放早于某时刻」的那一段
CREATE INDEX stored_tracks_last_played_idx
    ON stored_tracks (last_played_at);
