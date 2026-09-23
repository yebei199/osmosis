# playlists

`../playlists.rs` 的子模块目录,只放它的路由测试 `tests.rs`(与 `../likes/` 同形)。

- 负责:歌单那几条路由的端到端测试 —— 真 Postgres 加 `routes::testing` 的假 gRPC 上游。
- 不负责:路由本身(在 `../playlists.rs`)、存储层的测试(在 `server/tests/`)。
- 依赖:`crate::routes::testing` 的夹具,`catalog_cache` 的刷新时机常量。
