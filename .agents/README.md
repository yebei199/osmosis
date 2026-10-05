# .agents

给 AI 工作流工具读的仓库声明，不进产品构建。

- `services`：跑本仓库测试默认需要的本地服务，每行一个 `127.0.0.1:<端口>`，`#` 后是注释。remote-run 选机时会跳过缺这些服务的机器，门禁探测不通时报「未完成」，不会报通过（nixos_config#288）。改测试依赖的本地服务时，同步改这里。
- `derived`：生成出来、提交进仓库、不被任何编译或测试代码读的数据，每行一个 glob。门禁从
  base 读它，把这些文件从 rust test/coverage 与 accept 的复用输入里减掉（nixos_config#289）。
  目前只有覆盖地图 `test/remote-behavior/coverage-map.json`，它由每晚全量生成后单独提交。
