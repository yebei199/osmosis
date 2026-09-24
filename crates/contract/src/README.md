# contract/src

客户端与服务端之间的线上格式:HTTP 的请求体与响应体，以及信令 WebSocket 上的报文。
只放数据形状与少量纯函数(比如日志里怎么称呼一条命令),不放逻辑。

- `sync.rs`:信令两个方向的报文(`ClientSignal`、`ServerSignal`)与设备名册;
- `remote.rs`:遥控命令、被控端上报、迁移回话;
- `group.rs`:播放组的共同计划(主端发布，成员照着出声)与校时报文(#137 ⑤);
- `queue.rs`:服务端队列;
- `catalog.rs`、`playlist.rs`、`download.rs`、`account.rs`、`netease.rs`:各自那一类 HTTP 接口。

改动已有字段的语义就要升 `PROTOCOL_VERSION`(见 `lib.rs`)。
