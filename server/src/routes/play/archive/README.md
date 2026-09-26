# routes/play/archive

`archive.rs` 的测试(`tests.rs`,保留与上限那部分在 `tests/retention.rs`),挨着被测代码放,与 `routes/library/likes/` 同一个套路。

它们打真 Postgres(`just pg`),对象存储用 `routes/testing.rs` 里的内存替身,
上游是那里的假 bang-dream,音频字节由测试自己在随机端口上摆出来。
S3 客户端本身对真 RustFS 的测试不在这里,在 `server/tests/objects.rs`。
