# radio

`../radio.rs` 的子模块与测试。

- `filter.rs`:电台带着的筛选(#166)怎么判一首歌过不过,口径与客户端
  `app_core::facets` 对拍。
- `shared_tests.rs`:账号一份的共享电台歌单(#186):加载新歌追加并通知各台、
  听过的挪进「已听过」、新歌排进预取。
- `tests.rs`:对真实 Postgres 加进程内假上游,验听过过滤、不够再拉、拉取上限、
  按筛选过滤,以及心动模式交给上游的种子与红心歌单 id(#159)。

路由本身在 `../radio.rs`,听过的查询在 `server/src/store/history.rs`。
夹具(假上游、账号、库)来自 `crate::routes::testing`。
