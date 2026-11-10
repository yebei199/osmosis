-- 组里在放的这一段对应的起播事件(play_events 的行号)。这一段结束时按它补记听了多久,
-- 补记完就清空,同一段至多补记一次;没起过播(暂停着跳过去的、从独奏接过来的)是 NULL,
-- 结束时什么都不补。之前按「这首最近的一行」去认,会认到刚为下一段新记的那一行。
ALTER TABLE play_groups
    ADD COLUMN play_event_id BIGINT REFERENCES play_events (id) ON DELETE SET NULL;
