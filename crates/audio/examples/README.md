# examples

audio crate 的可运行探针：拿真 `Player` 开真声卡流，给 `test/` 下的端到端检查当被测进程。
不放单元测试(那在 `src/**/tests.rs`),也不放给用户用的工具。只依赖本 crate 公开的接口。

- `load_probe.rs`:放一路正弦波，每秒报一次累计欠载(xrun)数。`test/audio-rt-load.sh`
  在满载下跑它，验送声线程是实时策略、欠载为 0(#192)。
