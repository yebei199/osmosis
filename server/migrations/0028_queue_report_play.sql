-- 检查点带上在放的这一段:哪一条起播事件、到这一刻累计听了多久(毫秒)。独奏重启后恢复时
-- 接着这一行累计,跨着重启听够了也算听过;还没报上起播时两列都是 NULL。
ALTER TABLE play_queue_reports
    ADD COLUMN play_event_id BIGINT,
    ADD COLUMN listened_ms BIGINT;
