# syncplay/tests

走真信令的集成测试：服务端起在测试进程里(`server::syncplay::signaling` 的免鉴权测试路由),
几台「设备」是同一进程里的几个 `Client`。

- `signalling.rs`、`wss_signalling.rs`、`auth.rs`:建连、握手、TLS、鉴权;
- `client.rs`:编排循环的重连与名册;
- `group.rs`:校时走一遍真信令。组的全局状态要落库,对着真库的测试在 `server/tests/groups.rs`。
