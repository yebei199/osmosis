# radio

`../radio.rs` 的测试,别无他物。对真实 Postgres 加进程内假上游,验听过过滤、
不够再拉、拉取上限,以及心动模式交给上游的种子与红心歌单 id(#159)。

不放实现:路由在 `../radio.rs`,听过的查询在 `server/src/store/history.rs`。
夹具(假上游、账号、库)来自 `crate::routes::testing`。
