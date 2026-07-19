# test/e2e —— 浏览器端到端测试

bun + Playwright,驱系统装的 Chrome。跑:

```sh
just web-test          # 会先确认 dist/web 是最新的
```

或者在本目录直接 `bun install && bunx playwright test`。

## 三个绕不开的环境条件

这套测试量的是**真实的 GPU 行为**,所以不能在无头环境里跑,也进不了 CI:

1. **有头。**headless 的 Chrome 没有 WebGPU,`requestAdapter()` 恒为 `null`。
2. **窗口真在前台。**后台或被遮挡的标签页,`requestAnimationFrame` 会被压到 1Hz ——
   这不是假设,是排查时实测到的:第一次跑出来 1.0fps,聚焦窗口后 144.9fps。
3. **真 GPU。**软件光栅跑不出有意义的数。

任何一条不满足,测试会 **skip 而不是报红** —— 判据是对照组自己跑不到刷新率(见下)。

## frame-rate.spec.ts

回答:**web 端 3D 页每帧的开销,相对这台机器的天花板是多少?**

绝对阈值(`>= 100fps`)在这里没有意义,数字取决于显示器刷新率和这块 GPU。所以每次运行
先跑一个**对照组**:一张只做 clear 的 WebGPU canvas,形态复刻 Slint 页(1605×1984、
CSS 1.25x、`rgba8unorm`、`opaque`、每帧 2 次 submit)。它给出"这台机器上一个 WebGPU
页面能跑多快、GPU 该花多少",应用再跟它比。对照组同时是环境哨兵。

被测组走 `/?tab=2` 直接落在 3D 页 —— 界面整个画在一张 canvas 上,没有 DOM 元素可以点,
按坐标点导航栏会在界面一改之后**静默地量错页面**。

2026-07-19 实测(144Hz 屏):

| | 对照组 rafprobe | 应用 3D 页 |
| --- | --- | --- |
| 帧率 | 144.1fps | 59.0fps |
| GPU 进程占用 | 20% | **91%** |
| GPU 每帧 | 1.42ms | **15.48ms** |

**这个测试目前是红的,而且应该是红的** —— 它复现的是一个还没修的 bug:3D 页把 GPU 进程
占到了 91%。排查过程见 [`docs/wasm/frame-rate.md`](../../docs/wasm/frame-rate.md)。修好之后
它自己会变绿。

## trace.ts

录一段 Chrome trace 并算出帧的成本结构(帧率、主线程占用、GPU 进程占用、每帧 GPU 时间)。

两条口径必须守住,都是踩出来的:

- **按本页的 renderer pid 过滤 `GPUTask`。**GPU 进程的事件是全浏览器共享的,不过滤就会
  把别的标签页的开销算到自己头上。
- **窗口长度从数据里取**(本页首末 rAF 之间),不要拿事件计数去除一个想当然的秒数。

判断 GPU 是否吃紧看**占用率**,不看单次 `GPUTask` 的中位数:那个分布是长尾的,中位数会
把一条跑满的 GPU 线程读成"很闲"。
