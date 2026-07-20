# 交接:web 帧率修复与安卓闪黑排查(2026-07-19/20)

换机器接手用。这里记的是**当前状态、每个分支在哪、下一步怎么接**。
过程与数据在 [`../wasm/`](../wasm/) 下三份文档里,本文只在需要时指过去。

## 一句话

web 端 3D 页从 39fps 提到 134fps,根因是 femtovg 的 wgpu 后端在 wasm 上每个绘制元素要
花约 0.5ms;已修好并提了上游。安卓端另有一个触摸时闪黑的问题,查清了触发条件与责任方,
改进了一个数量级但**没有归零**,停在一个未合并的分支上。

## 一、三个仓库的状态

本地路径是这台机器上的;换机器把三个都克隆下来即可,分支全部已推送。

| 仓库 | 远端 | 本地路径 | 当前分支 |
| --- | --- | --- | --- |
| 主项目 | `yebei199/slint_study` | `~/RustroverProjects/slint_study` | `dev`,另有 `fix/glass-in-bevy-render-graph` |
| femtovg fork | `yebei199/femtovg` | `~/RustroverProjects/femtovg-fork` | 见下 |
| slint fork | `yebei199/slint` | `~/RustroverProjects/slint-fork` | `dev` |

依赖是这样串起来的,**主项目里只有 slint 一条 `[patch.crates-io]`**:

```
slint_study ──patch──> yebei199/slint@dev ──git 依赖──> yebei199/femtovg@perf/wgpu-resident-buffers
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

## 三、未完成:安卓触摸闪黑

**过程文档:[`../wasm/native-regression-2026-07-19.md`](../wasm/native-regression-2026-07-19.md)。**

### 现象

3D 页上触摸时,一片黑从某一行往下盖到内容区底部,那一行逐帧移动,看着像黑条扫下去。
桌面没有。

### 已确认

录屏逐帧算行亮度,判据是"大片变暗且一直暗到内容区底部":

| 条件 | 帧数 | 黑到底 |
| --- | --- | --- |
| 3D 页,自转、不触摸 | 238 | 1 |
| 3D 页,拖模型 | 240 | 11 |
| 3D 页,拖工具条 | 243 | 10 |
| Home 页,拖动 | 236 | 0 |
| 3D 页,玻璃 pass 关掉 | 259 | 0 |

触摸是触发条件,画面在动不是;任何位置的触摸都触发。render3d 侧打桩实测,拖动全程
**0 次 resize、0 次取不到纹理、0 次重新导入**,每帧交给 Slint 的都是同一张有效纹理。

### 已否掉的

- **不是这次会话改出来的。**安卓构建路径上只多了一行启动页签的属性赋值。
- **不是 femtovg 那套改动。**安卓走 skia,`cargo tree -i femtovg` 是空的。
- **不是读-改-写。**玻璃 pass 的 `LoadOp::Load` 换成 `Clear` 后从 10/243 降到 3~6/250,
  没有消除。这条否定结果**同时否掉了双缓冲**:它同样只缩小撞车窗口而不针对机制。

### 当前分支的方案与结果

`fix/glass-in-bevy-render-graph`(**未合并**,已推送):把玻璃后处理从"自建纹理 + 自建
`queue.submit`"改成 bevy 0.19 的 `FullscreenMaterial`,长在 bevy 自己的管线里。净删 239 行。

| 版本 | 黑到底 |
| --- | --- |
| 原始 | 10 / 243 |
| `Clear` | 6, 3, 3 |
| **本分支** | **5 / 约 1230**(5 轮:2, 0, 1, 2, 0) |
| 参照:玻璃关掉 | 0 / 259(**只采了一轮**) |

降一个数量级,但没到参照组的 0,所以**没有合并**。

### 下一步(按顺序)

1. **补采参照组。**"残留 5 次"整个建立在玻璃关掉那一轮的 0 上,一轮 259 帧不足以当基线。
   装回玻璃关掉的构建采 3 轮;若参照组本身也是 0~2 波动,那本分支就是修好了,可以合。
2. 若参照组稳定在 0,残留就是真的。链路现在简单多了(一张纹理、bevy 单一写入方),
   下一步该查 Slint 侧的采样时序,大概率要带着数据问上游。
3. 无论哪种结果,把结论补进 `native-regression-2026-07-19.md`。

## 四、换机器要准备的

### 复现安卓测量

设备是小米 13(`adb serial 5a61be0f`)。`adb devices` 报 `no permissions` 时,
`adb kill-server && adb start-server` 就够,不必动 udev 规则。

```sh
just android-build-3d && adb install -r dist/slint-study-debug.apk
adb shell am start -n io.github.slintstudy/.MainActivity
adb shell wm size                       # 1080x2400
adb shell input tap 895 2196            # 底部导航第三项 = 3D 页
```

量帧率必须让画面真在动:静止时 MIUI 把刷新降到 2fps,那不是缺陷。

闪黑的测法:

```sh
adb shell screenrecord --time-limit 8 --bit-rate 8000000 /sdcard/x.mp4 &
sleep 1; for i in $(seq 14); do adb shell input swipe 500 300 800 300 250; done
adb pull /sdcard/x.mp4
```

再用 ffmpeg 抽帧、按行算亮度,统计"变暗超过 80 行且最低行到内容区底部"的帧。
判据必须**大片 + 到底**,只按"变暗行数"会把画面本身的变化算进去(踩过)。

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
