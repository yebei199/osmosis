# server/tests

服务端 lib(`server::…`)的集成测试。每个文件一个主题,各自编成一个测试二进制。

- 打真依赖的:`accounts`、`cache`、`history`、`playlists`、`queues`、`signal_auth`
  连真 Postgres(`just pg`);`objects` 连真 RustFS(`just rustfs`)。两个容器都是
  `ci-test` 配方的前置,CI 的 test job 也各起一份。
- 不要外部进程的:`live_signaling` 在测试进程里自己起服务端。
- 默认 `#[ignore]` 的:`live_bangdream` 要一个跑着的 bang-dream,手动跑。

不测 HTTP 路由:路由在二进制 crate 里,集成测试引不到,它们的测试挨着源码放在
`src/routes/**/tests.rs`,夹具在 `src/routes/testing.rs`。
