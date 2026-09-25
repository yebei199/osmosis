# routes/play/links

`links.rs` 的测试(`tests.rs`),挨着被测代码放,与 `archive/` 同一个套路。

只测命中规则本身:时间由测试传 `Instant`,不连数据库、不起上游。
「第二次 `/play` 不再问上游」那条走整条路由,在上一级的 `play/tests.rs`。
