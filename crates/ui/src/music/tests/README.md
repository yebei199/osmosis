# music/tests

`music` 模块按主题分出去的集成测试,由 `music/tests.rs` 以 `mod` 挂进来。
每个文件一条用户路径:`dispatch`(一条播放意图从按下到落地)、`remote`(遥控与
被控两端)、`views`(浏览视图各存各的,#137 ④)。

不放纯规则的单元测试 —— 那些贴着被测模块写;也不放跨 crate 的端到端,那在
仓库根的 `test/`。共用的窗口与 `Deck` 夹具在 `music/fixtures.rs`。
