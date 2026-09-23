# server/tests —— 服务端集成测试

每个文件是一个独立的测试二进制,从外面用 `server` 这个 lib 起路由、连库、开 WebSocket。
单元测试不在这里,它们跟着被测代码住在 `server/src/` 里。

| 文件 | 验什么 | 要什么 |
|---|---|---|
| `accounts.rs`、`history.rs`、`playlists.rs`、`queues.rs`、`cache.rs` | 存储层对着真库的行为 | Postgres(`just pg`) |
| `signal_auth.rs` | `/signal` 的鉴权与来源校验 | Postgres |
| `live_signaling.rs` | 两条真实连接之间信令真的过去了 | 无,进程内起服务端 |
| `roster_log.rs` | 默认 `info` 下读得出「设备入册」(#113) | 无 |
| `live_bangdream.rs` | 对着真实 bang-dream 的联机测试,`#[ignore]` | 跑着的 bang-dream |

**抓日志的测试各占一个二进制。** `tracing::subscriber::set_default` 只管本线程,
与同一进程里并行的别的测试放在一起时一行都抓不到 —— `roster_log.rs` 就是这么
从 `live_signaling.rs` 里搬出来的。
