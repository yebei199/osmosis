-- 赞踩多一档「特别喜欢」(ADR 0042):verdict 从 {1, -1} 放宽到 {-1, 1, 2}。
-- 未表态仍是「没有这一行」,值域里照旧没有 0。
--
-- 0018 的 CHECK 是写在列上的匿名约束,Postgres 给它起的名字是
-- track_feedback_verdict_check;换掉它,不动 0018 那个文件(见本目录 README)。

ALTER TABLE track_feedback DROP CONSTRAINT track_feedback_verdict_check;
ALTER TABLE track_feedback
    ADD CONSTRAINT track_feedback_verdict_check CHECK (verdict IN (-1, 1, 2));

COMMENT ON COLUMN track_feedback.verdict IS
    '-1 踩,1 赞,2 特别喜欢;未表态没有行。喜爱度的锚,见 server::score 与 docs/adr/0042';
