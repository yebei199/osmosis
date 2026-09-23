# crates/api/src

**管什么**:把领域意图翻译成一次网络往返,以及紧挨着这些往返的本机落盘 ——
会话、设置、设备 id、封面、列表的本地缓存(`cache.rs`,#123)。native 与 wasm
的运行时差异在 `platform/` 里吸收,不向上传播(`docs/adr/0002`)。

**不管什么**:界面怎么摆、什么时候取(那是 `crates/ui`);领域规则(`crates/app-core`,
它不认识网络);线上格式的定义(`crates/contract`)。

**依赖**:`contract` 给线上类型;`reqwest` + `tokio` 做原生端的网络;`rusqlite`
(bundled)只给原生端的列表缓存。
