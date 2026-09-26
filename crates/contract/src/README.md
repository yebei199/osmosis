# contract/src

客户端与服务端之间的线上格式:HTTP 的请求体与响应体，以及信令 WebSocket 上的报文。
只放数据形状与少量纯函数(比如日志里怎么称呼一条命令),不放逻辑。

- `sync.rs`:信令两个方向的报文(`ClientSignal`、`ServerSignal`)与设备名册;
- `remote.rs`:播放状态与输出路由这两个小枚举(遥控报文已删,#142);
- `group.rs`:播放组的全局状态、组意图(`/group/*` 的请求体)与出声设备的执行事实(#142);
- `queue.rs`:服务端队列;
- `catalog.rs`、`playlist.rs`、`download.rs`、`account.rs`、`netease.rs`:各自那一类 HTTP 接口。

改动已有字段的语义就要升 `PROTOCOL_VERSION`(见 `lib.rs`)。
