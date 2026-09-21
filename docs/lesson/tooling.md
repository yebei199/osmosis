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
- **几棵 worktree 共用一个 `target/`,会互相覆盖工作区 crate 的产物。** cargo 的单元哈希用的是
  **相对工作区**的路径,于是 `94-signal-auth` 与 `96-netease-qr` 两棵树里的 `crates/api` 落进
  同一个槽位(实测同为 `libapi-bdaf7d3a43baf0e3`),谁后编谁覆盖,另一边随后报「找不到刚加的
  符号」而源码里明明有——2026-09-19 一天里三个实施者各吃了两次这种假红,也可能反过来假绿
  (链进去的是对方那一版)。共用 target 是为了省几十分钟的全量重编(见
  `memory` 里 worktree 必须复用主目录 target 那条),所以不改这个安排,改的是纪律:
  **同一时刻只让一棵树在编**;真要并行,`cp -a --reflink=always` 克隆一份私有 target
  (btrfs 上 106G 十几秒、几乎不占空间)再 `touch` 全量源码逼它重编工作区 crate。
- **后台任务里经 nix 环境跑 cargo,客户端报的退出码不可信。** 2026-09-21 把一条
  `nix-shell … --run 'cargo test --workspace --all-targets --no-run'` 放进后台任务,
  完成通知说 exit 0,而日志里明明是两个 `error[E0308]` 加一句
  `error: could not compile`。照那个退出码往下走,等于拿一棵编不过的树去跑下一步,
  而下一步的失败看起来像是新改动引起的。判据只认日志:命令末尾自己接一句
  `echo "EXIT=$?" >> 日志`,以日志里那一行为准,不以任务通知为准。整轮 #109 里
  这么用了十几次,没再误判过一次。与上面「重构建命令不要接管道」是同一类 ——
  中间多一层,真相就会在那一层被吃掉。
