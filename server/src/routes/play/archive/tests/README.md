# routes/play/archive/tests

`archive/tests.rs` 按主题拆出去的测试子模块(`docs/adr/0026`)。夹具(假上游、内存桶、
记账的小工具)留在上一级的 `tests.rs`,这里 `use super::*` 取用。

- `retention.rs`:留多久与放不下时谁让位 —— 清扫的保留规则、名次、空间上限的取舍、统计入口。
