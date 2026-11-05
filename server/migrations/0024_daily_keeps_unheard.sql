-- 日推从「最近一批」改成累积(ADR 0037):没听过的留到听完,和电台歌单同一套逻辑。
--
-- day 是推荐日:网易云北京时间 6 点换批,所以用 UTC+2 的日期,与客户端 daily_day 一致。
-- id 记批内次序。这张表从此不再整批替换。

ALTER TABLE daily_picks
    ADD COLUMN id BIGSERIAL,
    ADD COLUMN day DATE NOT NULL
        DEFAULT ((now() AT TIME ZONE 'UTC' + interval '2 hours')::date);

CREATE INDEX daily_picks_order_idx ON daily_picks (account_id, day DESC, id);
