# 交接:web 帧率修复与安卓闪黑排查(2026-07-19/20)

换机器接手用。这里记的是**当前状态、每个分支在哪、下一步怎么接**。
过程与数据在 [`../wasm/`](../wasm/) 下三份文档里,本文只在需要时指过去。

## 一句话

web 端 3D 页从 39fps 提到 134fps,根因是 femtovg 的 wgpu 后端在 wasm 上每个绘制元素要
花约 0.5ms;已修好并提了上游。安卓端另有一个闪黑问题,07-20 深夜定位到 Skia 的绘制与
wgpu 的 present 之间缺少顺序约束,证据充分,**决定不修**:发生率千分之一,肉眼看不出来,
而真修复要动 slint fork 的 semaphore 交接。待办详情见
[`../ready_issue/slint-wgpu-present-order.md`](../ready_issue/slint-wgpu-present-order.md)。

## 一、三个仓库的状态

本地路径是这台机器上的;换机器把三个都克隆下来即可,分支全部已推送。

| 仓库 | 远端 | 本地路径 | 当前分支 |
| --- | --- | --- | --- |
| 主项目 | `yebei199/osmosis` | `~/RustroverProjects/osmosis` | `dev`,另有 `fix/glass-in-bevy-render-graph` |
| femtovg fork | `yebei199/femtovg` | `~/RustroverProjects/femtovg-fork` | 见下 |
| slint fork | `yebei199/slint` | `~/RustroverProjects/slint-fork` | `dev` |

依赖是这样串起来的,**主项目里只有 slint 一条 `[patch.crates-io]`**:

```
osmosis ──patch──> yebei199/slint@dev ──git 依赖──> yebei199/femtovg@perf/wgpu-resident-buffers
```

`Cargo.lock` 当前锁定:

- slint `a82945f7f161b9cbcd92d63391798c85f3bd5163`
- femtovg `e2e321c036cbd8688d8f28f322f6516554844fd0`

### femtovg fork 的分支

| 分支 | 基底 | 用途 |
| --- | --- | --- |
| `perf/wgpu-resident-buffers` | 上游 v0.25.1 | **主项目在用的那条**。0.25.1 是因为 slint 1.18 依赖 `^0.25` |
| `pr/wgpu-cache-static-bindings` | 上游 master(0.26) | 上游 PR #303 |
| `pr/wgpu-per-draw-allocations` | 叠在上一条上 | 上游 PR #302 |
| `examples/many-rects` | 上游 master | 上游 PR #304 |

三条 0.26 分支的代码内容与 0.25.1 那条一致,只是基底不同。

## 二、已完成:web 端帧率

**结论文档:[`../wasm/femtovg-wgpu-per-element-cost.md`](../wasm/femtovg-wgpu-per-element-cost.md)**
(现象、最小复现、根因、四轮优化的数据)。排查过程与三个方法论错误在
[`../wasm/frame-rate.md`](../wasm/frame-rate.md)。

结果:3D 页 **39 → 134fps**;最小复现里每元素成本 **约 500µs → 1.2µs**(GL 是 0.72µs)。

### 上游

| | 内容 | 状态 |
| --- | --- | --- |
| [issue #305](https://github.com/femtovg/femtovg/issues/305) | 只讲事实,不带方案,问维护者想怎么修 | 待回应 |
| [PR #304](https://github.com/femtovg/femtovg/pull/304) | `examples/many_rects.rs` | 正式,待评审 |
| [PR #303](https://github.com/femtovg/femtovg/pull/303) | 缓存静态 sampler/view | draft |
| [PR #302](https://github.com/femtovg/femtovg/pull/302) | uniform buffer 那四个提交,叠在 #303 上 | draft |

两个改动 PR 刻意留 draft,描述里写明"想先在 issue 里把方向定下来"。维护者一旦回应,
按他的意见推进或重做。

### 还没做完的

- `test/e2e/frame-rate.spec.ts` **仍是红的**:帧率断言已过(93% ≥ 底线 70%),
  但 GPU 每帧仍是天花板的 3.9 倍、超出 3 倍上限。
- **空载 2.11ms 对 GL 的 0.28ms** 没查。这是现在最大的一块,而且已知它既不是光栅化也不是命令记录(`vkQueueSubmit` 只占 0.06ms)。
- 上游合入后要把 fork 与 patch 一并删掉。

## 三、已定位不修:安卓闪黑

**过程文档:[`../wasm/native-regression-2026-07-19.md`](../wasm/native-regression-2026-07-19.md)。**

### 现象

3D 页上触摸时,一片黑从某一行往下盖到内容区底部,那一行逐帧移动,看着像黑条扫下去。
桌面没有。

这一节 07-19 写的结论几乎全被 07-20 深夜的加大采样推翻,下面是修正后的版本。
被推翻的原因是同一个:那批对照每组只有 238 到 259 帧,而真实发生率是千分之一的量级,
那个样本量里测到 0 是常态。

### 原因

Skia 通过 `as_hal` 从 wgpu 里掏出裸 `VkQueue`,直接往交换链图像上画,wgpu 看不见这次提交。
它随后发出的 present 等的是一个 Skia 从未 signal 的 semaphore,绘制与扫描输出之间没有顺序
约束,合成器可以在 GPU 还没写完时就把图像拿走。

在 present 前阻塞 CPU 直到 Skia 画完,半截帧归零:0/11713,对照两组 dev 合计 23/21838
(0.105%),p 约 5e-6。代价是帧率从 92 掉到 50,所以它只是证据。

### 07-19 这几条结论都不成立

- **触摸是触发条件。**自转、全程不碰屏幕的那组同样出现。
- **玻璃关掉就是 0。**那轮只有 259 帧。加大采样后玻璃关掉照旧出现,9/1256,比开着还略高。
- **本分支把闪黑降了一个数量级。**分支里两个改动对闪黑的实测效果是零。重构本身仍然成立,
  理由是净删 239 行、去掉一次独立 `queue.submit` 和每帧重建的 view 与 bind group。
- **局部重绘与缓冲区轮换。**安卓走 wgpu-29 路径,`wgpu_29_surface` 没有覆写
  `use_partial_rendering()`,拿的是默认 `false`,每帧全窗口重画。

### 为什么不修

发生率 0.083% 到 0.13%,拖动时大约每 12 秒闪一下一帧,肉眼看不出来。真修复要让 wgpu 的
present 等在 Skia signal 的 semaphore 上,那是 slint fork 里的活,收益配不上工作量。

结论、复现步骤与探针 diff 收在
[`../ready_issue/slint-wgpu-present-order.md`](../ready_issue/slint-wgpu-present-order.md),
哪天想继续或想报上游,从那份文档接。

## 四、换机器要准备的

### 复现安卓测量

设备是小米 13(`adb serial 5a61be0f`)。`adb devices` 报 `no permissions` 时,
`adb kill-server && adb start-server` 就够,不必动 udev 规则。

```sh
just android-build-3d && adb install -r dist/osmosis-debug.apk
adb shell am start -n io.github.osmosis/.MainActivity
adb shell wm size                       # 1080x2400
adb shell input tap 895 2196            # 底部导航第三项 = 3D 页
```

量帧率必须让画面真在动:静止时 MIUI 把刷新降到 2fps,那不是缺陷。

闪黑的测法收在 `just android-flicker <标签> [秒数]`,默认录 120 秒(约一万帧)。
07-19 那套八秒录像加逐帧 magick 的做法已经弃用:样本量不够,而且慢十几分钟。

两条判据上的教训。样本量必须到万帧量级,几百帧测到 0 说明不了任何事,07-19 那一节
的结论就是这么错的。录制期间必须校验应用还活着,它中途被杀会把 MIUI 桌面录进去,
量出过一个 33.93% 的假数字。

### 复现 web 测量

`just web-test` 跑 `test/e2e/`,三条环境约束见 [`../../test/e2e/README.md`](../../test/e2e/README.md):
有头浏览器、窗口真在前台、真 GPU。探针在 `test/e2e/probes/`,`just web-test` 不跑它们。

量"画面出去了几帧"看 `Display::DrawAndSwap`,**不要拿 rAF 频率代替**。

### 两个静默失败(都踩过)

1. **shell 的工作目录会留在上一条命令的位置。**`cd test/e2e` 之后,下一条里的
   `git checkout crates/...`、`cp apps/web/index.html dist/web/` 全都失败;用 `&&` 串起来时
   整条链停住,**构建根本没跑**,产物停在上一版。
2. **测量前核对产物时间戳**(`ls -la --time-style=+%H:%M:%S dist/web/app_web_bg.wasm`)。
   改了源码却量到旧产物,症状与"改动无效"完全一样。复现页启动会打一行 `PROBE repro build
   ready`,探针认不到就直接失败,这道防线就是为此加的。

## 五、这次留下的可复用工具

| | 位置 |
| --- | --- |
| web 端到端测试与探针 | `test/e2e/`(`just web-test`) |
| 最小复现页(纯 Slint,`?rects=N`) | `apps/web/src/lib.rs` 的 `repro` 入口 |
| `?tab=` / `?bevy=off` | web 入口,长期保留的测试接缝 |
| 对照页 | `test/rafprobe.html`、`test/hostshape.html` |
| 给 femtovg 打桩的办法 | `../wasm/femtovg-wgpu-per-element-cost.md` 第七节 |
| 安卓半截帧测量 | `just android-flicker <标签> [秒数]` |
