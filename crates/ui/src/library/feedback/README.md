# feedback

`../feedback.rs` 的子模块目录,只放它的单元测试 `tests.rs`(与 `../liked/` 同形)。

- 负责:三态赞踩的乐观更新与投影,用无头窗口测(`i_slint_backend_testing`)。
- 不负责:模块本身(在 `../feedback.rs`)、网络请求(在 `crates/api`)。
- 依赖:`i_slint_backend_testing` 的无头窗口夹具。
