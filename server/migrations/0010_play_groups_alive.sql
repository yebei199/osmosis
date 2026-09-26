-- 组在放时,服务端最近一次确认自己还活着的挂钟时刻(#142 掉线规则 ②)。
--
-- 服务端挂掉时所有出声设备跟着断开,那一刻本该暂停,却没有进程去写。重启后按这一列
-- 补上:在放的组一律暂停,位置记在这一刻 —— 最后一台出声设备离开的那一刻,误差不超过
-- 一拍心跳。
ALTER TABLE play_groups ADD COLUMN alive_wall_us BIGINT;

-- boundary_wall_us 自 #142 AC-9 起存的是兜底推进的时刻(元数据放完 + 宽限),不再是放完的那一刻。
COMMENT ON COLUMN play_groups.boundary_wall_us IS
    '兜底推进的挂钟时刻:元数据时长放完再加宽限。出声设备先报放完就按报的推进';
