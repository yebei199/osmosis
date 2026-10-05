# 行为验收合同

这里保存 issue 的逐 AC 验收映射和统一运行声明。结果由 tdd 部署检查器核对真实
JUnit；遥控测试实现、素材、隔离资源与原始证据归 `test/remote-behavior/`，UI 几何回归归 `crates/ui/tests/`。
本目录不实现另一套校验规则，也不以映射声明代替执行结果。

`run.toml` 声明完整用例入口及本次结果落点；`181.toml` 保存不喜欢的匹配及真实用户路径合同；`186.toml` 保存共享电台歌单与 RustFS 每日清理合同，服务端那几条由 `test/remote-behavior/nextest.toml` 的投影打真 Postgres 跑；`175.toml` 保留遥控、播放和生命周期
回归合同，`177.toml` 保留顶部状态条与歌词头部合同。`run.sh` 的 `all` 运行累计套件，
`status-ui` 只运行本轮 UI 套件并生成 JUnit，`changed <base> <head>` 按改动挑累计子集（规则见 `test/remote-behavior/README.md`）。`just ci-remote` 与 forge 使用 Nix 固定引用的部署检查器消费这些文件。
当前材料仍处于审前，尚无功能通过结论。
