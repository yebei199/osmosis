# 待报:Skia 的绘制与 wgpu 的 present 之间缺少顺序约束

状态:**查清了,决定不修,也没报上游。**发生率千分之一,肉眼看不出来;真修复要动 slint
fork 里 semaphore 的交接,收益配不上工作量。这份文档按"哪天想报就直接拿去贴"的结构写,
同时也是自己将来接手的起点。

没报上游的理由:这条路径(`as_hal` 拿裸队列 + Skia 直接画交换链图像)是维护者自己设计的,
我没有深究他们在别处是否已经建立了顺序。带着一个可能看错了的结论去提 issue 会浪费别人的
时间。要报的话,先补上"还差什么才能定论"那一节。

## 环境

| | |
| --- | --- |
| slint | 1.18,fork 在 `yebei199/slint`,分支 `dev` |
| 特性 | `unstable-wgpu-29`,`backend-android-activity-06` |
| 渲染器 | skia,走 `wgpu_29_surface` |
| 设备 | 小米 13(`fuxi`),MIUI,1080x2400,120Hz 屏,Vulkan |
| 应用 | `yebei199/slint_study`,3D 页把 bevy 渲染的 wgpu 纹理交给 Slint |

## 现象

3D 页上,画面上半部分正常(工具条、玻璃面板、透出来的模型都在),从某条水平线往下整片空白,
切口每帧位置不同。**连 Slint 自己画的底部标签栏都会一起消失**,而 Android 系统导航栏正常。
底部标签栏是普通 Slint 控件,不碰共享纹理,它跟着消失说明这是一张只画了一部分的画面,
而不是纹理被采样到了半写状态。

## 猜测的机制

Skia 通过 `as_hal` 从 wgpu 里掏出裸 `VkQueue`,往 `get_current_texture()` 给的交换链图像上
直接画。wgpu 看不见这次提交,它随后 `frame.present()` 发出的 present 等的是一个 Skia 从未
signal 的 semaphore。绘制与扫描输出之间因此没有顺序约束,合成器可以在 GPU 还没写完时就把
图像拿走,取到上半截有新内容、下半截还没写的一帧。

相关位置(fork 的 `internal/renderers/skia/wgpu_29_surface.rs`):

- `render()` 里 `callback(skia_surface.canvas(), ...)` 让 Skia 画,紧接着
  `self.flush_and_submit(gr_context)`,它只做 `gr_context.submit(None)`,不等待。
- 再往下就是 `frame.present()`。两者之间没有任何 GPU 侧的同步对象。

## 证据

在 `frame.present()` 之前插一行 `gr_context.flush_submit_and_sync_cpu()`,阻塞 CPU 直到
Skia 的 GPU 工作完成(fork 分支 `probe/wgpu-present-sync`,commit `116207d8`,共 11 行):

| 条件 | 帧数 | 半截帧 | 比率 | 帧率 | 最暗帧亮度 |
| --- | --- | --- | --- | --- | --- |
| 基线(dev) | 10895 | 9 | 0.083% | 92fps | 37.2 |
| 探针(present 前同步 CPU) | 11713 | 0 | 0% | 50fps | 64.3 |
| 回退探针,重装 dev | 10943 | 14 | 0.13% | 93fps | 37.2 |

两组 dev 合计 23/21838(0.105%)。按这个比率,探针的样本量里期望出现 12.3 次,实测 0 次,
p 约 5e-6。两组 dev 的最暗帧都是 37.2,与半截帧同一档;探针一万多帧里最暗 64.3,中位 68.7,
没有一帧接近半截帧的形态。

第三行是回退验证,排掉了"探针期间设备或系统状态变了"这类解释。

**探针确实编进了产物**:探针那版帧率 50,前后两组 dev 都是 92 和 93。每帧一次完整 GPU
往返正是这个代价。(后来在 fork 上补了 `9b28a73`,启动时打一行 `PROBE ACTIVE`,以后不必
再靠帧率反推。)

## 已排除

| 假设 | 怎么排除的 |
| --- | --- |
| 玻璃 pass 的写与 skia 的读撞车 | 玻璃整个关掉照旧出现,9/1256,比开着还略高 |
| 读-改-写 | `LoadOp::Load` 换 `Clear` 后没有消除。这条同时排除了双缓冲,它同样只缩小窗口 |
| 触摸是触发条件 | 自转、全程不碰屏幕的那组同样出现 |
| bevy 的写与 Slint 的读没隔开 | 驱动挪到 `AfterRendering` 测得 6/1534,与不挪的 5/1230 一样 |
| 局部重绘与缓冲区轮换 | `wgpu_29_surface` 没覆写 `use_partial_rendering()`,拿默认 `false`,每帧全窗口重画 |

## 还差什么才能定论

**探针同时把帧率砍了一半。**窗口变小本身就会压低撞车概率,所以现在还分不清是"补上了顺序"
还是"机会变少了"。要分开这两者,得做出一个保持帧率的顺序修复再测一次:让 wgpu 的 present
等在 Skia 那次提交 signal 的 semaphore 上,而不是阻塞 CPU。

**没查上游是否在别处已经建立了顺序。**报 issue 之前必须先读明白 `as_hal` 这条路径的设计意图。

## 复现

探针原本在 fork 的 `probe/wgpu-present-sync` 分支上,该分支已删除,补丁正文见下,
重跑时手工打回 fork 的 dev 即可。`internal/renderers/skia/wgpu_29_surface.rs` 的
`render()` 里,`pre_present_callback` 那段之前插入:

```rust
// PROBE: block until Skia's GPU work has finished before presenting.
//
// Skia renders through the raw VkQueue pulled out of wgpu with `as_hal`, so wgpu never
// sees that submission and the present it issues waits on a semaphore that Skia never
// signalled. Nothing orders scanout after the drawing. On android that shows up as
// frames whose lower part is blank, cut at a row that moves between frames.
//
// Syncing the CPU is the wrong fix, it costs a full GPU round trip per frame. It is here
// to establish whether that missing order is what produces the blank frames.
gr_context.flush_submit_and_sync_cpu();
```

另有一行启动时打 `PROBE ACTIVE` 的日志,用来确认补丁真的编进了产物。不打这行就只能
靠帧率反推,而帧率减半这个特征只在这个探针上成立,换个探针就不适用了。

```sh
# 基线
just android-build-3d && just android-install
adb shell am start -n io.github.slintstudy/.MainActivity
just android-flicker baseline 120

# 探针:把上面那段打进 fork 的 dev,推上去
cargo update -p slint -p slint-build     # 每个被 patch 的包都要点名,少一个就静默变成 patch.unused
just android-build-3d && just android-install
just android-flicker probe 120
```

补丁作用于 `wgpu_29_surface.rs`。fork 追平上游之后这个文件可能已经改名或重写,重跑前
先确认 `as_hal` 那条路径还是同一个形状。

两条教训。样本量必须到万帧量级,真实发生率是千分之一,几百帧测到 0 是常态,07-19 那批
结论就是这么错的。录制期间必须校验应用还活着,它中途被杀会把桌面录进去,量出过一个
33.93% 的假数字,`just android-flicker` 现在自带这道校验。

## 过程文档

- [`../wasm/native-regression-2026-07-19.md`](../wasm/native-regression-2026-07-19.md)
- [`../sys/2026-07-19-web-frame-rate-and-android-flicker.md`](../sys/2026-07-19-web-frame-rate-and-android-flicker.md)
