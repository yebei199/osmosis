# feedback

`../feedback.rs` 的子模块目录,只放它的路由测试 `tests.rs`(与 `../likes/`、
`../playlists/` 同形)。

- 负责:赞踩那几条路由的端到端测试 —— 真 Postgres,不问上游(赞踩不碰网易云)。
- 不负责:路由本身(在 `../feedback.rs`)、存储层的测试(在 `server/tests/`)。
- 依赖:`crate::routes::testing` 的夹具。
