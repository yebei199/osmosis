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
**bevy 全关、只画一个 Slint UI 的情况下,每帧仍吃掉 GPU 线程 12.8ms。** 这个数字本身
就不合理,是当前最值得追的线索。

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

## 五、下一步

方向已从"rAF 请求发少了"转到"**一帧占用太久,且 CPU 与 GPU 完全串行**"。

1. **先防第四次口径错误。**用 `test/rafprobe.html`(能跑 145Hz)录一份 trace,拿同一个
   脚本算它的 GPU 进程占用。若它也是 70%,说明这个指标区分不了快慢,结论作废;若它只有
   个位数,那"每帧 12.8ms GPU 线程"就坐实了。
2. 坐实之后,拆开那 12.8ms:重录一份带 `disabled-by-default-gpu.service` / viz 分类的
   trace,看每帧那 ~3 个 `GPUTask` 具体在干什么(纹理导入、拷贝、present)。
3. `?scale=`、MSAA、GPU 负载这批"无效"结论是在 16ms 天花板下测的(错误一),拆掉节流后
   **从未复测**。要用它们之前先重做。

## 六、附:排查用的开关与工具

`test/rafprobe.html` 保留在仓库里(`just web-dev` 会复制进 `dist/web/`),用法见
`test/README.md`。

排查期间用过的临时 URL 开关(`?scale=`、`?bevy=`、`?redraw=`、`?shadow=`)结论落定后
均已撤除,不在代码里。要重做同类实验时照着 git 历史加回即可。

后台构建与产物验证的坑另见 [`AGENTS.md`](../../AGENTS.md) 的「后台构建」一节——
本次排查有四轮因为验证的是旧产物而作废。
