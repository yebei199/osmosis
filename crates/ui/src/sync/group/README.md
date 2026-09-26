# sync/group

界面这一侧的播放组（#142）：本机在服务端全局播放状态里是什么身份，点歌、控制条要改成发给服务端的意图，以及「与 X、Y 一起播放」这类显示。

- `../group.rs`：接线。
  - 收信令事件：`GroupState`、`DeviceReport`、连上和断开。
  - 发意图：`api::group_*`。
  - 推界面属性：横幅、输出设备那一排、组那一行。
- `rules.rs`：纯文案与判断，不起窗口就能测。

**不负责**：
- 本机怎么跟着全局状态出声：归 `music::playback::group`。
- 规则本身，比如本机算不算出声设备、此刻该放哪一条：归 `app_core::GlobalGroup`。
- 信令连接与名册：归 `../link.rs`。

**依赖**：`app_core`、`api`、`syncplay`、`crate::notice`。
