# web 端 3D 帧率排查(2026-07-19)

现象:桌面 144fps、安卓 120fps,而 **web 端 3D 页只有 ~39fps**。本文记录整个排查过程、
已排除的因素、三次导致长时间走错方向的方法论错误,以及当前的结论。

排查未完成。写下来是因为期间做的测量工具和排除结论都能复用,而三个方法论错误值得钉死。

## 一、当前确定的事实

第三次重新解析 DevTools trace 后(前两次的口径都是错的,见第四节)。窗口取本页首末 rAF
之间,按本页 pid 过滤:

| | 带 bevy(`110504`) | `?bevy=off`(`152841`) |
| --- | --- | --- |
| rAF 请求 | 49.0/s | 68.3/s |
| rAF 触发 | 49.0/s(id 连续,零丢失) | 68.3/s(同) |
| `BeginFrame` | 129.8/s | 142.5/s |
| `BeginMainThreadFrame` | 41.4/s | 63.0/s |
| **`DrawFrame`(真画出来的帧)** | **39.0/s** | **56.9/s** |
| 主线程 `RunTask` 占墙钟 | 54% | 30% |
| **本页 GPU 进程占墙钟** | **67%** | **73%** |
| GPU 工作与 rAF 回调重叠 | 31% | 13% |

两个结论:

**1. 帧周期 ≈ 主线程时间 + GPU 进程时间。两者几乎不重叠。**

- `?bevy=off`:CPU 1492ms/280 帧 = 5.3ms,GPU 3589ms/280 帧 = 12.8ms,合计 **18.1ms**;
  实测周期 1/56.9 = **17.6ms**。
- 带 bevy:CPU 13.7ms + GPU 17.2ms = 30.9ms;实测 25.6ms(有 31% 重叠,对得上)。

相加就是周期,这是**没有流水线**的签名:每帧主线程干完再等 GPU 干完,两段之和超过一拍
(144Hz = 6.94ms),于是只能落到第 2 拍甚至第 3 拍。

**2. 关掉 bevy,GPU 进程占用不降反升(67% → 73%)。**

所有 `GPUTask` 都跑在 GPU 进程的**同一条线程**上,单个 task 的 p90 有 7.6~10.4ms。
**bevy 全关、只画一个 Slint UI 的情况下,每帧仍吃掉 GPU 线程 12.8ms。**

**3. 同口径的对照页排除了"这台机器本来就这样"。**

用同一个解析脚本录了 `test/rafprobe.html` 的 trace(同机、同屏、同一个 Chrome,
canvas 复刻 Slint 页的形态:1605×1984、CSS 1.25x、`rgba8unorm`、`opaque`、每帧 2 次
submit),实测 144.9fps:

| | rafprobe(144.9fps) | 我们的页面 `?bevy=off`(56.9fps) |
| --- | --- | --- |
| GPU 进程占墙钟 | **27%** | **73%** |
| 主线程占墙钟 | 12% | 30% |
| `GPUTask` 个数 / 帧 | 3.0 | 3.2 |
| **GPU 时间 / 帧** | **1.85ms** | **12.8ms** |
| rAF 间隔 | 6.94ms(720 帧里 719 帧落在 7ms) | 17.6ms |

**每帧的 GPU 任务个数一样,单个任务贵了 7 倍。** 这是目前最值得追的线索。

这套对照已经固化成自动化测试(`test/e2e/frame-rate.spec.ts`,`just web-test`),不再需要
人开浏览器抄数字。它跑出来的数比上面手录的更狠——同一次运行内:

| | 对照组 rafprobe | 应用 3D 页(`?tab=2`) |
| --- | --- | --- |
| 帧率 | 144.1fps | 59.0fps |
| GPU 进程占用 | 20% | **91%** |
| GPU 每帧 | 1.42ms | **15.48ms** |

**一个页面把 GPU 进程占到 91%。** 这个测试目前是红的,而且应该是红的:它复现的就是这个
还没修的 bug,修好之后自己会变绿。

## 一之二、已经排除的方向:rAF 请求链路

曾经的主线假设是"我们每秒只发起 54 次 rAF 请求,串行自举错过当拍"。这个假设有两处错:

- **54/s 是算错的**(分母没按窗口取,见第四节错误三)。真实值是 49/s 与 68.3/s,
  且请求数与触发数**逐帧相等**。
- **请求并不滞后。**trace 里"触发 → 下次请求"的中位延迟是 **0.49ms**,即请求发在回调
  开头,不是等一帧画完之后。

winit 源码印证了这一点。`winit-0.30.13/src/platform_impl/web/web_sys/animation_frame.rs`
的 `request()` 同步、无条件地调 `window.requestAnimationFrame`,没有队列、没有 `setTimeout`、
没有微任务跳;Slint 在 `about_to_wait`(`internal/backends/winit/event_loop.rs`)里就把下一帧
订好了。**帧 N+1 在帧 N 的回调返回之前已经排队。**

所以瓶颈不在"什么时候请求下一帧",而在"一帧要占多久"。

## 二、已排除的因素(全部有实测)

| 因素 | 怎么排除的 |
| --- | --- |
| 3D 渲染分辨率 | `?scale=` 从 1 降到 0.25(像素少 ~50 倍),帧率不变 |
| 画布/窗口尺寸 | 窗口缩到 3D 纹理仅 37×214,帧率不变 |
| GPU 光栅成本 | 同上两条;且 trace 显示 GPU 每帧仅 4ms |
| bevy 全部工作 | `?bevy=off` 完全不驱动渲染器,仍 ~60fps |
| 玻璃 pass | 每帧 0.07ms |
| MSAA | 关闭(`Msaa::Off`)无变化 |
| 管线同步编译 | `app.update()` 稳态仅 3.1ms,不存在每帧重新编译 |
| 浏览器插件 | 无痕模式结果相同 |
| canvas 配置 | 尺寸、CSS 缩放、alphaMode、format、usage 逐项复刻到对照页,全 145Hz |
| device 形态 | 逐字复刻 wgpu 的 `requestDevice` descriptor,145Hz |
| 每帧提交次数 | 复刻"每帧 2 次 submit",145Hz |
| Slint 的 16ms 帧节流 | 已在 fork 中拆除(见第三节),拆后仍 60Hz |
| 重绘请求时机 | `?redraw=after` 挪到 `AfterRendering`,无变化 |
| wgpu 版本 | 升 30 不可行:bevy 0.19 锁 `wgpu = 29.0.3`,而纹理共享要求两边同一份 crate |

对照页 `test/rafprobe.html` 证明:**同浏览器、同尺寸的 WebGPU 画布持续呈现能稳定 142~145Hz**,
所以限制不来自浏览器、WebGPU 或显示器。

## 三、期间做的改动

已合入(都经三端实测):

- **帧驱动从 `Timer` 改为渲染通知**(`a556ae2`)。原先 wasm 上用 16ms 定时器驱动,
  硬性设了 62fps 上限;改用 `set_rendering_notifier(BeforeRendering)` 后由真实重绘周期派发。
  桌面 144 / 安卓 120 / web 40→50。
- **每帧耗时日志**(`ccc17cd`、`aacbc15`)。render3d 侧拆出 `app.update()` 与玻璃 pass;
  ui 侧把一帧拆成「回调 / Slint 渲染 / 空等」三段。**这两套是整个排查里唯一持续有效的工具。**
- **`test/rafprobe.html`**(`89ce438` 起)。纯 HTML+JS 的对照页,用来判定"是浏览器的锅还是我们的锅"。
- **slint fork 拆掉 wasm 上的帧节流定时器**(`7a2109a` 锁定 `yebei199/slint@efe9f79`)。
  `internal/backends/winit/frame_throttle.rs` 的 `TimerBasedFrameThrottle` 用显示器刷新率算间隔,
  而 web 后端报不出刷新率,回退 60Hz(`unwrap_or(60000)` → 16ms)。web 上 `request_redraw`
  本就由 rAF 驱动,再套定时器既多余又设了上限。**这是真实的 60Hz 上限,但拆掉后帧率没变**——
  它是必要条件而非充分条件。

已撤销(编译通过、逻辑合理、实测无效):`Msaa::Off`、重绘请求挪到 `AfterRendering`、
3D 面板按 1x 渲染、关闭工具条阴影。

## 四、三个方法论错误(这才是重点)

### 错误一:在天花板下做减法实验,把否定结果当排除证据

Slint 的 16ms 帧节流把帧率压在 60Hz。在它被拆掉之前,**所有"减少工作量"的实验必然显示"无效"**
——天花板压着,GPU 降到 0 帧率也不会涨。而我据此逐一"排除"了分辨率、MSAA、GPU 负载。

那批结论全是假的,却被我当成事实用了好几个小时,直接导致后面反复绕路。

**规则:存在已知上限时,不要从"改了没变化"推出"这个因素无关"。先拆上限,再重做实验。**

### 错误二:解析 Chrome trace 时没按进程过滤

Chrome 的 GPU 进程和 trace 是**全浏览器共享**的:`GPUTask` 带 `renderer_pid`,`AnimationFrame`
也来自所有标签页。我直接对全部事件求和求中位数,得到:

- GPU 每帧 9.95ms(实际本页 4.83ms)—— 于是误判"GPU 超预算导致降频"
- rAF 间隔 13.79ms(实际本页 6.87ms)—— 于是误判"浏览器只给 60~72Hz"

**这两个数是后续一整轮排查的前提,而它们都是错的。**基于它们做的实验(降分辨率、关 MSAA、
关阴影)都在解决不存在的问题。

正确做法:先用 `AnimationFrame` / `ProfileChunk` 的 pid 认出本页 renderer,再按
`args.data.renderer_pid` 过滤 `GPUTask`,按本页 `AnimationFrame` 算帧数。

```python
own = collections.Counter(e['pid'] for e in ev
                          if e.get('name') in ('AnimationFrame', 'ProfileChunk'))
mypid = own.most_common(1)[0][0]
g = [e['dur'] / 1000 for e in ev
     if e.get('name') == 'GPUTask' and 'dur' in e
     and e.get('args', {}).get('data', {}).get('renderer_pid') == mypid]
```

### 错误三:分母拍脑袋,以及拿中位数当预算

改对了 pid 过滤之后,第二轮的数还是错的,两个新毛病:

- **事件计数除了个想当然的时长。**"339 次 = 54/s"隐含 6.28s,而窗口的真实长度要从事件
  自己的时间戳取(首末 rAF 之间 4.90s / 4.92s)。真实值是 49/s 和 68.3/s。
- **拿 `GPUTask` 的中位数当"每帧 GPU 预算"。**这个分布是长尾的:p50 只有 1.24ms,
  p90 却有 10.4ms。该看的是**占墙钟的比例**(67% / 73%),中位数把一个跑满的 GPU 线程
  读成了"很闲"。

**规则:比率的分子分母都要从数据里取;判断资源是否吃紧看占用率,不看单次中位数。**

同一份 trace 被解析了三次,三次结论互相矛盾——这不是 trace 的问题,是每次都换了一个
没有验证过的口径。改口径之后先重算旧结论,再往下推。

## 五、那 15.5ms 拆开是什么

带上 `disabled-by-default-gpu.dawn` 分类重录,并按**独占时间**(减掉子区间,否则最外层
的壳会排在最前面)聚合 GPU 进程里落在我们帧区间内的事件:

| 事件 | 独占 | 每帧 |
| --- | --- | --- |
| `WebGPUDecoderImpl::HandleDawnCommands` | 2438ms(53%) | 8.2ms |
| `Queue::Submit` | 1686ms(37%) | 5.7ms |
| `vkQueueSubmit`(真正下到驱动) | 61ms | **0.2ms** |

**真实的 GPU 驱动工作只有 0.2ms/帧,其余全是 Dawn 在 CPU 侧解命令、做校验。** 命令流为什么
这么大,调用次数给了答案:

```
DeviceBase::APICreateRenderPipeline    9.7 次/帧
CommandEncoder::Finish                 9.7 次/帧
ShaderModuleVk::GetHandleAndSpirv      整个窗口只有 4 次
```

**每帧新建约 10 条渲染管线,而着色器一共只编译了 4 次。** 不是在重新编译着色器,是反复用
同样的着色器重新创建管线对象 —— 每条都要把一个巨大的 pipeline descriptor 序列化过线、
再由 Dawn 解出来并校验。这就是那 8.2ms。

管线创建随帧数走:同样 5 秒,不重绘的 tab 0 建了 77 条,3D 页建了 2860 条。

### 每帧重建管线是真的,但它不是瓶颈

给 femtovg 打桩(`[patch.crates-io]` 指向本地副本,在管线缓存的命中/失配处打日志)之后,
机制清清楚楚 —— 稳态每帧重复:

```
PROBE miss: FillColorUnclipped …
PROBE render 结束: 缓存 10 条,本次用到 1 条     ← 这次 render 只用了 1 条,retain 丢掉另外 9 条
PROBE miss × 9  (FillColor / FillImage / TextureCopy…)
PROBE render 结束: 缓存 10 条,本次用到 9 条     ← 又把那 1 条丢掉
```

**一帧里有两次 femtovg `render()`,用的管线集合不相交,而 femtovg 的淘汰策略是"本次
`render()` 没用到就丢"**(`femtovg-0.25.1/src/renderer/wgpu.rs:538`)。两次互相清空对方。

第一次 `render()` 只有一条命令,那是 Slint 在**装了 rendering notifier 时**提前 flush 的
窗口背景 `clear_rect`(`internal/renderers/femtovg/lib.rs:223`)。两个前提我们都占:3D 帧
驱动装了 notifier,窗口背景是纯色。

把那句 `retain` 改成只在缓存超过阈值时才扫,重建现象消失了 —— **而帧率一点没动**:

| | 修前 | 修后 |
| --- | --- | --- |
| `APICreateRenderPipeline` | 9.9 次/帧 | 榜上无名 |
| `HandleDawnCommands` 独占 | 9.50ms/帧 | 9.32ms/帧 |
| `Queue::Submit` 独占 | 7.18ms/帧 | 7.10ms/帧 |
| 帧率 | 59.0fps | 59.6fps |

每帧重建 10 条管线只值 **0.19ms/帧**。这是个真实的上游缺陷,但不是我们要找的东西。

### 那 20ms 不随像素量变化,所以它多半是在等

`viewport-sweep` 把视口从 1280×900 扫到 200×150(像素少 40 倍):

| 视口 | 帧率 | GPU 进程占用 | GPU 每帧 |
| --- | --- | --- | --- |
| 1280×900 | 41.9fps | 90% | 21.80ms |
| 640×480 | 48.2fps | 88% | 20.63ms |
| 320×240 | 46.9fps | 91% | 20.97ms |
| 200×150 | 49.1fps | 91% | 20.37ms |

**完全不动。**加上 `vkQueueSubmit` 只有 0.2ms/帧、`CommandBufferVk::RecordCommands` 只有
0.2ms/帧,结论只能到这一步:**每帧有约 20ms 卡在 GPU 进程里,而它既不是光栅化,也不是
命令记录。** 剩下的可能是同步等待(等上一帧的栅栏、等 wire 缓冲、等共享 device),
这一步还没证。

注意"GPU 进程占用 91%"这个指标本身包含**阻塞时间**。前面据它说"GPU 是瓶颈"是过度解读:
成立的只有"每帧 20ms 花在 GPU 进程里",至于是干活还是等,占用率区分不了。

### 大头不是 bevy,是 Slint 自己

`?bevy=off` 跳过驱动渲染器但照常请求重绘,界面以同样的节奏画,只是不含 bevy 那份工作:

| | 帧率 | GPU 进程占用 | GPU 每帧 |
| --- | --- | --- | --- |
| `?tab=2` | 39.4fps | 86% | 22.41ms |
| `?tab=2&bevy=off` | 58.0fps | **89%** | **17.91ms** |
| `?tab=0`(不重绘) | 1.0fps | 3% | — |

bevy 只值 4.5ms/帧。**bevy 全关、只画一个静态 Slint 界面,GPU 进程仍占 89%、每帧 17.9ms。**

### 工作量本身是微不足道的

给 femtovg 打桩数每次 `render()` 的命令量,稳态每帧只有:

```
PROBE render: 2 条命令, 0 个 drawable, 6 个顶点      ← 提前 flush 的窗口背景 clear
PROBE render: 13 条命令, 12 个 drawable, 626 个顶点  ← 整个界面
```

**13 条命令、626 个顶点。** 解这么点命令不可能花 9.3ms。到此"在等"已无可辩驳。

### 等在 `Queue::Submit` 里

把最长的 `GPUTask` 整个展开(`probes/inside-wait.spec.ts`):

```
GPUTask (32.86ms)
  └ … └ WebGPUDecoderImpl::HandleDawnCommands (32.51ms)
        ├ CommandEncoder::Finish (0.01ms)
        └ Queue::Submit (32.18ms)          ← 时间全在这里
            ├ Queue::ValidateSubmit        (0.00ms)
            ├ CommandBufferVk::RecordCommands (0.05ms)
            ├ vkQueueSubmit               (0.02ms)   ← 真正下到驱动的
            └ DawnServiceSerializer::Flush (0.02ms)
```

**32.18ms 花在 Dawn 的 `Queue::Submit` 内部、任何子跨度之外**,而子项加起来只有 0.1ms。
32ms ≈ 2×16.7ms,16.7ms 正是 60Hz 一拍。这是在等呈现。

### 应用页的拍子只有对照页的一半

| | 对照页 rafprobe | 应用页 `?tab=2&bevy=off` |
| --- | --- | --- |
| `DelayBasedBeginFrameSource::OnTimerTick` | 125.3/s | **57.0/s** |
| `Display::DrawAndSwap` | 123.8/s | 48.5/s |
| `FireAnimationFrame` | 218.8/s | 48.5/s |

两页走的是同一种 BeginFrame 来源,但给应用页的拍子只有一半多一点。**因果方向还没定**:
是拍子先降到 60、于是提交被迫等一拍;还是我们提交得慢、Chrome 据此降了频。

## 六、下一步

已经被排除的(**都有实测,且都在天花板拆掉之后**):

| 假说 | 怎么排除的 |
| --- | --- |
| rAF 请求发少了 / 错过当拍 | 请求延迟中位 0.49ms;winit 在回调里同步排下一帧 |
| 每帧重建 10 条渲染管线 | 打桩证实了机制,修掉之后帧率不动(只值 0.19ms/帧) |
| 模糊阴影(`drop-shadow-blur: 40px` 等) | 全部置 0 重编,57.4fps / GPU 90%,不动 |
| 光栅化像素量 | 视口缩 40 倍,GPU 占用 90% → 91%,不动 |
| bevy 的开销 | `?bevy=off` 后 GPU 仍占 89%、每帧 17.9ms;bevy 只值 4.5ms/帧 |
| 命令量大 | femtovg 每帧只发 13 条命令、626 个顶点 |

**问题已经收敛成一句话:同一个浏览器里,一张裸 WebGPU 画布能拿到 125/s 的拍子并且提交
不阻塞,而 Slint 的画布只拿到 57/s 并且每次提交要等一到两拍。差别在哪。**

1. **定因果方向。**拍子先降,还是提交先慢?可做的实验:让 Slint 隔帧才请求重绘(把负载
   减半),看拍子回不回到 125/s。回去了说明是 Chrome 因为跟不上而降频,是结果;不回
   说明拍子本来就只给这么多,是原因。
2. **比对两张画布的差别。**文档第二节记过一轮"逐项复刻 canvas 配置到对照页,全 145Hz",
   但那轮复刻的是 **canvas 的配置**,没有复刻 **wgpu 建 surface 的方式**(swapchain 张数、
   usage、是否走离屏再拷贝)。差别多半在这一层。
3. 复测 MSAA —— 这批"无效"结论同样是在 16ms 天花板下得出的(错误一),尚未重做。

## 七、附:排查用的开关与工具

`just web-test` 跑 `test/e2e/` 那一套(bun + Playwright + 系统 Chrome),用法与三条环境
约束见 [`test/e2e/README.md`](../../test/e2e/README.md)。三个 spec 各司其职:
`frame-rate` 断言相对天花板的成本,`gpu-breakdown` 按独占时间拆 GPU 进程开销,
`viewport-sweep` 分辨"在干活"还是"在等"。

`test/rafprobe.html` 保留在仓库里(`just web-dev` 会复制进 `dist/web/`),现在是
`frame-rate` 对照组的宿主页。

`?tab=` 是长期保留的测试接缝:界面整个画在一张 canvas 上,没有 DOM 元素可点,按坐标点
导航栏会在界面一改之后静默量错页面。`?bevy=off` 让 web 入口跳过驱动渲染器但照常请求
重绘,用来把 bevy 的开销和 Slint 自己的分开。其余临时开关(`?scale=`、`?redraw=`、
`?shadow=`)结论落定后已撤除,照着 git 历史加回即可。

### 给 femtovg 打桩的办法

`[patch.crates-io]` 里加一行指向本地副本即可,slint 会跟着用上:

```toml
femtovg = { path = "../femtovg-debug" }
```

副本从 `~/.cargo/registry/src/*/femtovg-0.25.1` 拷出来,`chmod -R u+w`。femtovg 自己依赖
`log`,所以直接 `log::info!` 就能经 `console_log` 打到浏览器控制台,Playwright 那侧用
`page.on('console')` 收。

顺带记一个上游缺陷:`femtovg-0.25.1/src/renderer/wgpu.rs:538` 每次 `render()` 结束都把
"本次没用到"的管线丢掉。只要一帧里有两次用不同管线集合的 `render()`,两次就互相清空。
对我们只值 0.19ms/帧,没提上游。

后台构建与产物验证的坑另见 [`AGENTS.md`](../../AGENTS.md) 的「后台构建」一节——
本次排查有四轮因为验证的是旧产物而作废。
