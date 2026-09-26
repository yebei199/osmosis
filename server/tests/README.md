# server/tests —— 服务端集成测试

每个文件是一个独立的测试二进制,从外面用 `server` 这个 lib 起路由、连库、开 WebSocket。
单元测试不在这里,它们跟着被测代码住在 `server/src/` 里。

| 文件 | 验什么 | 要什么 |
|---|---|---|
| `accounts.rs`、`history.rs`、`playlists.rs`、`queues.rs`、`cache.rs` | 存储层对着真库的行为 | Postgres(`just pg`) |
| `groups.rs` | 组的全局播放状态:意图、广播、掉线暂停、重启后恢复且版本不回退(#142) | Postgres |
| `signal_auth.rs` | `/signal` 的鉴权与来源校验 | Postgres |
| `objects.rs` | S3 客户端:存、取(含 Range)、删、不可达(#126) | RustFS(`just rustfs`) |
| `live_signaling.rs` | 两条真实连接之间信令真的过去了 | 无,进程内起服务端 |
| `roster_log.rs` | 默认 `info` 下读得出「设备入册」(#113) | 无 |
| `live_bangdream.rs` | 对着真实 bang-dream 的联机测试,`#[ignore]` | 跑着的 bang-dream |

Postgres 与 RustFS 两个容器都是 `ci-test` 配方的前置,CI 的 test job 也各起一份。

**抓日志的测试各占一个二进制。** `tracing::subscriber::set_default` 只管本线程,
与同一进程里并行的别的测试放在一起时一行都抓不到 —— `roster_log.rs` 就是这么
从 `live_signaling.rs` 里搬出来的。

不测 HTTP 路由:路由在二进制 crate 里,集成测试引不到,它们的测试挨着源码放在
`src/routes/**/tests.rs`,夹具在 `src/routes/testing.rs`。
