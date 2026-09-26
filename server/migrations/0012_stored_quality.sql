-- 缓存改存无损(#147,docs/adr/0034):每个对象记下实际拿到的音质。
--
-- quality 仍是向音源要的档位(键的一部分),tier 是音源实际给的档位。
-- 旧的行要的是 'high' 也拿到 'high',照抄过来;它们随后被清扫任务整批删掉,
-- 因为缓存只留 'lossless' 要来的那些。
--
-- 位深与采样率只有看过文件头才知道(FLAC 的 STREAMINFO),不知道就是 NULL。

ALTER TABLE stored_tracks ADD COLUMN tier TEXT;
UPDATE stored_tracks SET tier = quality;
ALTER TABLE stored_tracks ALTER COLUMN tier SET NOT NULL;

ALTER TABLE stored_tracks ADD COLUMN bits_per_sample INTEGER;
ALTER TABLE stored_tracks ADD COLUMN sample_rate INTEGER;
