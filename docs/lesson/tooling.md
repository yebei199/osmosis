# 开发工具

踩过的坑,一个主题一个文件。写入由 `lesson` 技能负责,它只在用户明确要求时运行。

- **slint 内嵌 MCP 的元素树会跨进程粘连,不能拿它判「元素不存在」。** `just desktop-dev` 起的
  实例被 kill、`justfile` 里那个 8091 端口都释放之后,`list_windows` 仍然应答;重启后查唯一
  元素 `MainWindow::viz-anchor-card` 返回三个句柄,句柄编号跨进程重启一路递增到 246。据此
  判定 `MainWindow::seek-slider` 与 `MainWindow::lyric-entry` 不在树里是错的,那两个元素在
  `cargo test -p ui` 的无头树里点得到、拖得动。元素在不在以 `crates/ui/tests/` 里
  `i-slint-backend-testing` 的查询为准,MCP 只用来看截图。
