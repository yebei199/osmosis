# 开发工具

踩过的坑,一个主题一个文件。写入由 `lesson` 技能负责,它只在用户明确要求时运行。

- **slint 的 MCP 有两个端点,认错一个就会把手机当成桌面。** `.mcp.json` 里
  `slint-app` 指 8091(桌面),`slint-android` 指 8090(`adb forward` 到手机)。会话启动时
  桌面没跑,工具列表里就只剩 `slint-android`,而它报的窗口是 1080x2400、scale 2.75,
  看起来和一个竖着的桌面窗口没有区别。2026-08-13 据此认定「桌面上新加的
  `MainWindow::seek-slider` 与 `MainWindow::lyric-entry` 不在元素树里」,查了一小时、
  重编了三次,真相是查的一直是手机,而手机上那个包是这些元素写出来之前打的。
  连的是哪一台,拿 `adb shell wm size` 与工具报的窗口尺寸对一下就知道。
- **元素在不在,以 `crates/ui/tests/` 里 `i-slint-backend-testing` 的
  `find_by_element_id` 为准,不以 MCP 为准。** MCP 只能证明「你连的那份树里没有」,
  证不了「代码里没有」。无头测试查的是当前源码编出来的树,没有装错包、连错端口的余地。
