-- 组的全局播放状态(#142):每个账号至多一个组,一行即一个组。
--
-- 服务端是这份状态唯一的权威:组里任何设备的点歌、切歌、暂停、拖动都在一个事务里
-- 读锁这一行、改写、version + 1,再广播出去。落库是为了服务端重启之后组照样在、
-- 版本号不回退。
--
-- 组散了(成员走光)不删行:version 要接着往上数,新的组不能拿到比旧组更小的版本。

CREATE TABLE play_groups (
    account_id BIGINT PRIMARY KEY REFERENCES accounts (id) ON DELETE CASCADE,
    version BIGINT NOT NULL,
    -- 加入了组、能控制的设备;outputs 是其中真正出声的那几台
    members TEXT[] NOT NULL DEFAULT '{}',
    outputs TEXT[] NOT NULL DEFAULT '{}',
    -- 此刻放的是组队列哪一版的哪一条。组里还没有歌时三列都空
    queue_id BIGINT REFERENCES play_queues (id) ON DELETE SET NULL,
    revision BIGINT,
    entry_id BIGINT,
    playing BOOLEAN NOT NULL DEFAULT false,
    -- 时间线:挂钟 anchor_wall_us(微秒)这一刻,媒体在 position_us。
    -- 存挂钟而不是服务端单调钟:单调钟每次启动从零起,重启后换算不回来
    position_us BIGINT NOT NULL DEFAULT 0,
    anchor_wall_us BIGINT NOT NULL DEFAULT 0,
    -- 这一首放完的挂钟时刻,只在播放时有。续播任务按它挑出该往下推的组
    boundary_wall_us BIGINT,
    shuffled BOOLEAN NOT NULL DEFAULT false,
    loop_mode TEXT NOT NULL DEFAULT 'off',
    -- 随机时的播放次序(条目号);不随机时为空,次序就是队列原序
    play_order BIGINT[] NOT NULL DEFAULT '{}'
);

CREATE INDEX play_groups_due ON play_groups (boundary_wall_us)
    WHERE playing;
