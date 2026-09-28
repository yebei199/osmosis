# blocks

`../blocks.rs` 的子模块目录,只放它的路由测试 `tests.rs`(与 `../tags/` 同形)。

- 负责:屏蔽规则那几条路由的端到端测试 —— 真 Postgres 加 `routes::testing` 的假 gRPC 上游。
- 不负责:路由本身(在 `../blocks.rs`)、存储层实现(在 `server/src/store/blocks.rs`)。
- 依赖:`crate::routes::testing` 的夹具。
