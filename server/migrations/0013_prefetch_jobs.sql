-- 预取队列(#147):该以无损存进桶的曲目,排队等后台 worker 去取。
--
-- 一首歌一行,主键就是它的身份 —— 重复入队撞在主键上,不会多出第二行。
-- 取完(存进桶)即删行;留下来的行都是还没办成的,统计入口据此数排队与没存的。
--
-- worker 用 FOR UPDATE SKIP LOCKED 领任务,领走时把 run_after 推到租约到期:
-- 进程半路死掉,租约一过别的 worker 能重新领,不必另记「谁领了」。

CREATE TABLE prefetch_jobs (
    platform TEXT NOT NULL,
    track_id TEXT NOT NULL,
    -- 以谁的音源凭据去取。几个账号先后放进来的同一首,留先到的那个
    account_id BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    -- queued:等着取;no_lossless:音源给不出无损;over_cap:超出空间上限没存;
    -- failed:重试用尽。后三种等下一次入队(歌单变动、日推、重启)再试
    state TEXT NOT NULL DEFAULT 'queued',
    -- 领过几次。失败的退避按它加长,到上限记 failed
    attempts INTEGER NOT NULL DEFAULT 0,
    -- 早于它不领:领走时推到租约到期,失败时推到退避之后
    run_after TIMESTAMPTZ NOT NULL DEFAULT now(),
    enqueued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (platform, track_id)
);

-- worker 按它找「到点了的排队任务」
CREATE INDEX prefetch_jobs_due_idx
    ON prefetch_jobs (run_after) WHERE state = 'queued';
