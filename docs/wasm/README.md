# docs/wasm

web / wasm 端特有的问题记录。这里放的是**在浏览器里才会遇到、且花过力气才搞清**的东西;
跨端通用的架构决策在 [`docs/adr/`](../adr/),Slint 本身的用法笔记在 [`docs/slint/`](../slint/)。

- [`frame-rate.md`](frame-rate.md) —— 3D 页帧率排查:已排除的十几项因素、两个把排查带偏
  数小时的方法论错误(天花板下做减法、解析 Chrome trace 不按进程过滤),以及当前指向的
  方向。附可复用的 trace 解析口径。
