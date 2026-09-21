-- 播放队列。**不是缓存**(见 docs/adr/0031):删了会丢用户自己攒的那批歌与顺序,
-- 所以 0018 那三条缓存规矩不管它,它也不外键指向 platform_tracks ——
-- 清一次平台缓存不能把用户的队列删掉、不能改顺序、不能把重复项合并。
--
-- 与 0002 的 local_playlist_tracks 的形状差别正是队列与歌单的差别:歌单的主键是
-- (歌单, 平台, 曲目),同一首歌只能有一条;队列允许同一首歌出现多次,所以这里的
-- 身份是 entry_id。

CREATE TABLE play_queues (
    id BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    -- 队列归属于一个播放会话 / 输出设备,不是直接拿 account_id 当唯一当前队列。
    -- 否则同账号两台设备各自本机播放会互相覆盖(ADR 0031 二)。
    device_id TEXT NOT NULL,
    -- 已提交的最新版本。修改原子产生新版本,读取固定在一个版本上 ——
    -- 不会前半页旧顺序、后半页新顺序。
    revision BIGINT NOT NULL,
    -- 下一个空闲的 entry_id。entry_id 要跨 revision 稳定,所以不能用行号:
    -- 行号会随插入删除整体挪位,而播放端手上那个 (applied_revision, entry_id)
    -- 就此对不上账。
    next_entry_id BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX play_queues_owner_idx ON play_queues (account_id, device_id);

-- 一个版本里的全部条目。改一次队列写一整份新版本的行,旧版本留着 ——
-- 播放端可能还停在旧版本上,而正在分页读的那一方也不能读到一半换了顺序。
CREATE TABLE play_queue_entries (
    queue_id BIGINT NOT NULL REFERENCES play_queues (id) ON DELETE CASCADE,
    revision BIGINT NOT NULL,
    -- 同一首歌在队列里重复出现时,区分的是哪一次出现。
    entry_id BIGINT NOT NULL,
    -- 展示原序。播放顺序是另一回事,它是执行状态,存在 play_queue_reports 里。
    position BIGINT NOT NULL,
    platform TEXT NOT NULL,
    track_id TEXT NOT NULL,
    -- 创建该版本时的展示信息快照。存它是因为曲目会下架、平台会故障、权限会
    -- 失去 —— 那时要保留条目身份、位置与最后已知的展示信息并显示不可用,
    -- 而不是像歌单刷新那样把缺详情的曲目从队列里剔掉。
    title TEXT NOT NULL,
    alias TEXT,
    artists TEXT[] NOT NULL,
    -- 封面 URL 存了也不保证永久有效。临时播放直链一律不存。
    cover TEXT,
    duration_ms BIGINT NOT NULL,
    PRIMARY KEY (queue_id, revision, entry_id)
);

-- 读一个版本就是按 position 扫这个前缀,建这条索引正是为它
CREATE INDEX play_queue_entries_page_idx
    ON play_queue_entries (queue_id, revision, position);

-- 第二层:待应用的播放意图。一个队列同时只有一条 —— 连点 A、B 时 B 覆盖 A,
-- 迟到的 A 因此再也应用不上(它带的 operation_id 已经不是这一条了)。
--
-- 单独一张表而不是 play_queues 上几个列:三层裁决分开是 ADR 0031 的决定,
-- 混进一张表迟早会有人拿「数据库里写了」当「已经出声」。
CREATE TABLE play_queue_intents (
    queue_id BIGINT PRIMARY KEY REFERENCES play_queues (id) ON DELETE CASCADE,
    -- 这条意图是发给哪台设备的。换目标之后旧设备的执行报告不该再作数。
    device_id TEXT NOT NULL,
    revision BIGINT NOT NULL,
    entry_id BIGINT NOT NULL,
    -- 重试同一次操作不该再次重置播放,所以它由发起方生成、服务端只比对。
    operation_id TEXT NOT NULL,
    -- pending / applied / failed。失败要留痕:下载失败时保留旧执行副本,
    -- 但不能谎报已应用。
    state TEXT NOT NULL,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 第三层:播放端最近确认的执行状态。**服务端存的是一份报告,不是实时真相** ——
-- 断连时它就是过期的,界面必须把它标成过期而不是假装实时。
CREATE TABLE play_queue_reports (
    queue_id BIGINT PRIMARY KEY REFERENCES play_queues (id) ON DELETE CASCADE,
    device_id TEXT NOT NULL,
    -- 执行端 epoch:播放端进程启动时的毫秒挂钟,重启换一个。与 state_seq 一起
    -- 组成 (epoch, state_seq) 这个可比的序,旧 epoch 或旧序号的迟到报告一律拒。
    epoch BIGINT NOT NULL,
    state_seq BIGINT NOT NULL,
    -- 播放端**实际应用**的版本。与 play_queues.revision 分开:下载失败时
    -- 它停在旧值上,而那正是要能看出来的事。
    applied_revision BIGINT NOT NULL,
    entry_id BIGINT,
    -- 实际播放次序:entry_id 的排列。显式存,不让两端凭 seed 猜同一个排列
    -- (ADR 0031 六)。空数组表示播放端还没报过。
    play_order BIGINT[] NOT NULL,
    -- 列表循环的轮次。随机每轮重洗,所以「第几轮」与排列要一起看。
    round BIGINT NOT NULL,
    position_ms BIGINT NOT NULL,
    play_state TEXT NOT NULL,
    reported_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
