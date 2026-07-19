# web 端 3D 帧率排查(2026-07-19)

现象:桌面 144fps、安卓 120fps,而 **web 端 3D 页只有 ~55fps**。本文记录整个排查过程、
已排除的因素、两次导致长时间走错方向的方法论错误,以及当前的结论。

排查未完成。写下来是因为期间做的测量工具和排除结论都能复用,而两个方法论错误值得钉死。

## 一、当前确定的事实

按**正确口径**(见第四节)重新解析 DevTools trace 后:

| | 带 bevy | `?bevy=off`(不驱动渲染器) |
| --- | --- | --- |
| 本页收到的 rAF 间隔 | 8.94ms(**112Hz**) | 6.87ms(**146Hz**) |
| 本页 GPU 每帧 | 4.83ms | 4.03ms |
| CPU 每帧(回调内) | 4.4ms | 1.1ms |
| Slint 渲染(CPU) | 0.9ms | 0.9ms |
| **实际画出的帧** | **~55fps** | **~60fps** |

**浏览器给的拍子够(112~146Hz),GPU 没超预算(4~4.8ms vs 144Hz 的 6.9ms),CPU 也没超。**
但我们每秒只画了约 55~60 帧。

trace 里主线程的事件计数指向了原因:

```
RequestAnimationFrame   339 次  (54/s)   ← 我们只请求了 54 次/秒
FireAnimationFrame      336 次  (54/s)   ← 请求几次触发几次,零丢失
BeginFrame              759 次  (122/s)  ← 浏览器实际有 122Hz 的拍子可给
```

**不是浏览器少给、也不是 Slint 丢拍,而是我们每秒只发起 54 次 rAF 请求。**

我们的帧循环是串行自举的:`BeforeRendering` 里请求下一帧 → 画完 → 下次 rAF 触发 → 再请求。
每轮都要等上一帧走完才发出下一次请求,于是错过当拍、落到再下一拍。这是下一步要验证的方向。

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

## 四、两个方法论错误(这才是重点)

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

## 五、下一步

验证"每秒只发起 54 次 rAF 请求"这个直接观测:

1. 确认 Slint 的重绘请求链路在 web 上是否天然错过当拍(串行自举 → 每 2~3 拍一帧)
2. 若属实,方向是让重绘请求**不依赖上一帧走完**——在 winit 或 slint 层让 rAF 保持排队

注意此时 `RequestAnimationFrame` 54/s 与 `BeginFrame` 122/s 的差距是**直接测量值**,
不是推断,可以作为验证基线。

## 六、附:排查用的开关与工具

`test/rafprobe.html` 保留在仓库里(`just web-dev` 会复制进 `dist/web/`),用法见
`test/README.md`。

排查期间用过的临时 URL 开关(`?scale=`、`?bevy=`、`?redraw=`、`?shadow=`)结论落定后
均已撤除,不在代码里。要重做同类实验时照着 git 历史加回即可。

后台构建与产物验证的坑另见 [`AGENTS.md`](../../AGENTS.md) 的「后台构建」一节——
本次排查有四轮因为验证的是旧产物而作废。
