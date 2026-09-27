# lyric_probe

`../lyric_probe.rs` 的测试,别无他物。歌词分类判据(`kind_of`)的单元测试,与
worker 单步(`step`)对真实 Postgres 加进程内假上游的集成测试。

不放实现:worker 在 `../lyric_probe.rs`,队列的 SQL 在 `server/src/store/lyric.rs`。
夹具(假上游、账号、库)来自 `crate::routes::testing`。
