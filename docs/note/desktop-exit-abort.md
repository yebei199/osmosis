# 桌面关窗后 abort:谁在 TLS 析构阶段丢了锁

debug 构建的桌面端,关窗后进程收尾必崩,退出码 134。运行期完全正常。
诊断过程见 issue #15;本文记的是**真正的根因、两条走不通的修法、以及最后怎么修的**。

## 一、症状

```
thread 'main' panicked at library/std/src/thread/local.rs:428:25:
cannot access a Thread Local Storage value during or after destruction: AccessError
fatal runtime error: thread local panicked on drop, aborting
```

复现条件比原报告写的简单得多:**开起来、关掉窗口**,就这样。不需要放音,不需要
打开播放页。原报告猜的是 rodio 的 `DeviceSink`,方向错了 —— 下面的调用栈里没有音频。

## 二、根因

core dump 里主线程的栈:

```
#5  std::sys::thread_local::abort_on_dtor_unwind::DtorUnwindGuard::drop
#6  std::sys::thread_local::native::eager::destroy<OnceCell<i_slint_core::context::SlintContext>>
#7  __call_tls_dtors
#8  __run_exit_handlers
#9  exit                      ← main 已经返回了
```

以及 panic 自己的 backtrace:

```
wgpu_core::snatch::LockTrace::enter          snatch.rs:94
wgpu_core::snatch::SnatchLock::read          snatch.rs:147
<wgpu_core::device::queue::Queue as Drop>::drop   queue.rs:236
```

拼起来是这样:

1. Slint 把 `SlintContext` 放在一个 `thread_local` 里。窗口、渲染器、以及我们挂上去的
   渲染通知闭包(连同 bevy `Scene` 与两条 wgpu pass)全吊在它下面。
2. `main` 返回后 glibc 跑 `__call_tls_dtors`,这棵对象图**这时候才**开始析构,
   于是 `wgpu::Queue::drop` 落在 TLS 析构阶段。
3. 它要取 `SnatchLock`。那把锁带一个递归获取的检测器,检测器自己也存在一个
   `thread_local` 里 —— 而那个 TLS **已经析构了**。读它 panic,drop 里 panic
   直接 `fatal runtime error` → abort。

两个 TLS 同在主线程,glibc 不保证析构顺序,谁先没谁背锅。

**只影响 debug 构建。** 那个检测器是 `#[cfg(all(debug_assertions, feature = "std"))]`
门控的(wgpu-core 29 `src/snatch.rs:64`),release 下编进去的是空壳。但开发全程跑
debug,core dump 会污染崩溃统计,所以还是得修。

## 三、两条走不通的修法

- **提前放掉自己那份 wgpu 句柄**(事件循环返回后拆掉渲染通知)。没用:Slint 的渲染器
  还持着一份 `Queue`,最后一次释放仍然落在 TLS 析构里。谁最后放手谁触发 `Queue::drop`,
  而最后那个不是我们。
- **`std::process::exit(0)`**。这条是直觉陷阱:它调的是 libc `exit()`,而
  `__run_exit_handlers` → `__call_tls_dtors` 正是崩溃发生的那一段,照跑不误。
  实测退出码仍是 134,与不加时一字不差。

## 四、修法:`_exit`

`apps/desktop/src/main.rs` 末尾,事件循环返回后直接 `libc::_exit(0)` —— 进
`exit_group`,退出处理器与 TLS 析构器一概不跑。std 没有对应的口子,所以多了一条
`libc` 依赖(只对 `cfg(unix)`)。

代价是析构函数不执行。核对过这代价具体是什么:

- 此刻要还的只有 GPU 与音频设备,内核回收得比我们干净。
- 全仓没有任何落盘路径(`crates/` 与 `apps/` 里没有 `fs::write` / `File::create`),
  没有东西要 flush。
- 日志不丢:env_logger 写 stderr,不带缓冲。

只改桌面端。web 没有这条退出路径;android 的进程由系统回收,`main` 不以返回收场。

## 五、回归验收

`just desktop-exit-check`:起一个实例、关掉、看退出码,要 0 不要 134。

这条不进 `just ci` —— 它要合成器给窗口、要显卡给 wgpu adapter,CI 里两样都没有。
但凡动过 `apps/desktop` 的收尾路径、或者升过 wgpu / slint,在本机跑一次。

## 六、这一类问题的通用形状

「在 TLS 析构器里析构一棵持有别人 TLS 的对象图」是个反复出现的坑,这次只是撞在
wgpu 上。修法之所以选**整体跳过收尾**而不是**逐个躲开**,就是因为下一个碰 TLS 的
析构函数随时可能从哪个依赖里冒出来 —— 本仓已经先后怀疑过 rodio、撞上过 wgpu。

## 更新记录

- 2026-07-30:定位到 wgpu-core 的 `SnatchLock` 递归检测器;确认 `std::process::exit`
  无效,改用 `_exit`;加 `just desktop-exit-check` 作回归验收。撤掉此前基于错误诊断
  加在 `crates/audio` 的 `log_on_drop(false)`。
