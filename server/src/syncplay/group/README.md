# syncplay/group

组的全局播放状态（#142）：每个账号至多一个组，服务端持有唯一一份状态。组里任何设备的点歌、切歌、暂停、拖动都是发来的意图，服务端定序、改写、递增版本，再广播给账号下的每台在线设备。

- `../group.rs`：接线部分。负责从库里读锁、应用意图、落库，然后通过名册广播 `GroupState`；设备出册、放完自动续播也在这里处理。
- `timeline.rs`：纯规则部分。包括时间线（锚点加位置）、上一首/下一首、放完自动推进、掉线暂停、随机次序。不读钟，也不碰库。
- `tests.rs`：`timeline.rs` 的测试。

**不负责**：
- 队列的定义与版本：归 `store::queue`，组只引用 `queue_id`、`revision`、`entry_id`。
- 落库的 SQL：归 `store::group`。
- HTTP 的形状：归二进制里的 `routes::group`。

**依赖**：`contract`（线上类型）、`store::{group, queue}`、`super::roster`、`super::clock`。
