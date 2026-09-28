-- 补记一次播放听了多久(#157)。0003 那条「只增不改」从这里起不再成立:
-- 客户端切歌/播完/停止时回填这两列,原始起播那一行不变,只是多两个字段。
-- 没回填(进程被杀)的行两列都是 NULL,算不出完播也算不出跳过,接受。

ALTER TABLE play_events ADD COLUMN listened_ms BIGINT;
ALTER TABLE play_events ADD COLUMN duration_ms BIGINT;
