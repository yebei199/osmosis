# syncplay/tests

走真信令的集成测试：服务端起在测试进程里(`server::syncplay::signaling` 的免鉴权测试路由),
几台「设备」是同一进程里的几个 `Client`。

- `signalling.rs`、`wss_signalling.rs`、`auth.rs`:建连、握手、TLS、鉴权;
- `client.rs`:编排循环的重连与名册;
- `remote.rs`:遥控器模式 —— 接管、命令、上报、被顶掉;
- `group.rs`:播放组(#137 ⑤)—— 校时收敛、组通告、共同计划经服务端转给跟随端。
