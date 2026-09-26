# routes/play/prefetch

`prefetch.rs` 的测试(`tests.rs`),挨着被测代码放,与 `archive/` 同一个套路。

它们打真 Postgres(`just pg`),验的是队列本身:重复入队只有一行、几个 worker
同时领只领走一个、没办成的再入队会重新排上、重试用尽记 failed。用各自独有的
平台名领任务,并行的别的测试排进来的网易云任务碰不到。worker 办一个任务
(存进桶、按结果删行或记原因)的测试在 `archive/tests.rs`,那里有假上游与内存桶。
