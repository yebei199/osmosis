# api/src

把领域意图翻成一次网络往返:登录、曲库、歌单、队列、下载、应用内升级。各端运行时的
差异(有无线程、能否阻塞)在 `platform/` 里吸收,对外的 `async fn` 在 native 与 wasm 上
签名相同(`docs/adr/0002`)。

- 每个端点族一个模块(`auth`、`catalog`、`playlists`、`queue`……),地址拼接集中在 `url`。
- `platform/` 是唯一按 target 分叉的地方;会话、设置、封面缓存的落盘也在那里。
- `update` 问的是 GitHub 而不是我们的服务端,请求不带登录态。

不管的:界面状态(归 `ui`)、领域规则(归 `app-core`)、线上格式(归 `contract`)。
依赖只有 `contract`。
