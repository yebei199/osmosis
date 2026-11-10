-- 检查点带上这一批是从哪儿起播的(`contract::QueueSourceDto`:日推、私人 FM、心动模式),
-- 独奏重启后恢复时据此接着跳过听过的、接着续电台。别处点的批次是 NULL。
ALTER TABLE play_queue_reports ADD COLUMN source JSONB;
