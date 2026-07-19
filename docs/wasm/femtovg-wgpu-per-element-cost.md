# femtovg 的 wgpu 后端在 wasm 上每个元素要花 0.5ms

结论文档。**怎么查出来的**、期间走过的弯路和三个方法论错误,在
[`frame-rate.md`](frame-rate.md)。

## 现象

web 端 3D 页只有 39fps,而桌面 144、安卓 120。表现出来像"Slint 在 web 上锁了 60"。

不是锁,是攒出来的:**每画一个界面元素,GPU 进程就多花约 0.5ms**。十来个元素凑够 14ms,
超过 144Hz 的一拍(6.94ms),于是只能隔一拍出一帧。

## 最小复现

```sh
just web-dev repro        # WebGL 渲染器
# 或者带上 wgpu:
nix-shell slint.nix --run 'cargo build -p app-web --target wasm32-unknown-unknown --release --features repro,wgpu'
```

页面是纯 Slint:一个矩形、一个永不停的动画驱动重绘,没有 3D、没有 bevy、没有本项目界面
(代码见 `apps/web/src/lib.rs` 的 `repro` 入口)。`?rects=N` 再铺 N 个**静止**的小矩形。
动的仍只有原来那一个,重绘节奏不随 N 变。

同一份代码,只换渲染器:

| 矩形数 | **WebGL**(`renderer-femtovg`) | **WebGPU**(`renderer-femtovg-wgpu`) |
| --- | --- | --- |
| 0 | — | 144.0fps / GPU 4.25ms |
| 200 | 144.0fps / GPU 0.41ms | 8.1fps / 103.06ms |
| 1000 | 144.0fps / GPU 1.02ms | 2.0fps / 434.46ms |
| 5000 | **144.0fps / GPU 1.25ms** | — |

WebGL 画 5000 个矩形每帧 1.25ms 且满帧;WebGPU 画 200 个就掉到 8fps。wgpu 那条路是线性的:

    (32.83−4.25)/50 = 0.57ms   (111.12−4.25)/200 = 0.53ms   (434.46−4.25)/1000 = 0.43ms

与圆角无关(200 个圆角 103ms、200 个直角 101ms),与像素量无关(视口缩 40 倍不变),
与元素类型无关(矩形、文字、SVG 路径都一样)。

测量环境:Chrome(系统装的)、144Hz 屏、NVIDIA lovelace,窗口在前台。量的是
`Display::DrawAndSwap`(真正呈现出去的帧),**不是 rAF 频率**。两者不是一回事。

## 根因:每个 draw 新建 6 个 GPU 对象

`femtovg-0.25.1/src/renderer/wgpu.rs`,`BindGroupState::materialize`(`:1363-1415`)与
`create_binding_resource_and_sampler`(`:1507-1558`)在每次 bind group 缓存未命中时创建:

| 对象 | 个数 | 位置 |
| --- | --- | --- |
| `create_buffer_init`(fragment uniform) | 1 | `:1370` |
| `create_sampler` | 2 | `:1526` |
| `create_view` | 2 | `:1555`(无图像时是 1×1 空纹理的 view) |
| `create_bind_group` | 1 | `:1389` |

而所谓"缓存"是**单槽的上一次值比较**(`:1725`),拿整个 224 字节的 `UniformArray` 做
`PartialEq`,不是 keyed cache:

```rust
if self.current_bind_group_state != Some(bind_group_state.clone()) {
    self.current_bind_group = bind_group_state.materialize(...).into();
    self.current_bind_group_state = Some(bind_group_state);
}
```

浏览器里数出来的调用次数与源码完全对上:

| 每帧创建 | rects=0 | rects=200 | 每个矩形 |
| --- | --- | --- | --- |
| buffer | 4 | 204 | **1.0** |
| bindGroup | 3 | 203 | **1.0** |
| sampler | 4 | 404 | **2.0** |
| texture view | 7 | 407 | **2.0** |
| command encoder | 1 | 1 | 0 |

**每个矩形都未命中。** 颜色相同也没用:位置不同 → `paint_mat`/`scissor_mat` 不同 →
uniform 数组不等。而那 2 个 sampler 和 2 个 view 是 1×1 空纹理的,**永远不变,纯属白建**。

在 wasm 上这六次创建都要跨 JS 边界进 GPU 进程做校验与分配,`create_buffer_init` 还要
alloc→map→copy→unmap 一整套。~0.5ms/元素就是这么来的,而且什么都没被摊销,所以线性。

## GL 后端为什么没事

`opengl.rs:275-291`,`set_uniforms` 在 drawable 循环**之外**调一次;`set_uniforms`
(`:467-507`)本身只是一次 `glUniform4fv`(14 个 vec4)加两次 `glBindTexture`。
没有 buffer、没有 descriptor、零分配。每个矩形约 0.25µs,是纯命令流写入。

wgpu 版每个矩形的开销是它的约 2000 倍。

## 影响

- 只影响 **wasm/WebGPU** 这条路(`renderer-femtovg-wgpu`)。原生上同样的代码不走 JS 边界,
  六次创建便宜得多,尚未测过。
- 本项目 web 端 3D 页 39fps、GPU 进程占用 91%、`Queue::Submit` 里每帧 16~32ms 空等,
  全部由此解释。
- 顺带解释了为什么"减少工作量"的实验全都无效:降分辨率、关阴影、关 MSAA 都不改变
  **元素个数**。

## 上游

已提 draft PR:[femtovg/femtovg#302](https://github.com/femtovg/femtovg/pull/302),
分支 `pr/wgpu-per-draw-allocations`,基于上游 master(0.26),五个提交与本项目在用的
那条一致,只是基底不同。本项目走的是 0.25.1 那条,因为 slint 1.18 依赖 `^0.25`。

接线不用 `[patch.crates-io]`:slint fork 的 `internal/renderers/femtovg/Cargo.toml`
直接指向 `yebei199/femtovg` 的 `perf/wgpu-resident-buffers`,本仓库只留 slint 一条 patch。

## 解法

1. **退回 WebGL 渲染器**(马上可用)。代价是失去 3D:纹理共享必须要 WebGPU,
   目前只能二选一。
2. **缓存不变的对象**:2 个 sampler 与 2 个空纹理 view 挂成 `WGPURenderer` 的字段,
   6→2。改动小、不碰绑定布局、无 API 变化。
3. **结构性修法**:整帧一个大 uniform buffer + `has_dynamic_offset: true`,每帧只一个
   bind group,6→0。**已实现并验证**,见下。

## 已验证:小修不够,必须做结构性那一步

`yebei199/femtovg` 的 `fix/wgpu-cache-static-bindings` 分支(从上游 v0.25.1 开,好让它满足
slint 对 `^0.25` 的依赖)实现了上面第 2 条:空纹理的 view 存到 `WGPURenderer` 上,
采样器按那三个真正影响 descriptor 的标志做缓存。本项目接进来复测:

| 每帧创建 | 修前(每矩形) | 修后(每矩形) |
| --- | --- | --- |
| sampler | 2.0 | **0.0** |
| texture view | 2.0 | **0.0**(恒定 3 个/帧) |
| buffer | 1.0 | 1.0 |
| bindGroup | 1.0 | 1.0 |

| | 修前 | 修后 |
| --- | --- | --- |
| 200 矩形 | 8.1fps / GPU 103.06ms | 9.9fps / GPU 97.12ms |
| 1000 矩形 | 2.0fps / GPU 434.46ms | 2.3fps / GPU 378.47ms |

**去掉 6 个对象里的 4 个,只换来约 10%。** 剩下的 `create_buffer_init` 与
`create_bind_group` 占了几乎全部开销。前者是 alloc→map→copy→unmap 四次跨进程操作,
后者要校验 5 个绑定项。

所以第 3 条不是"更好的做法",是**必需**:整帧一个大 uniform buffer(按 256 对齐攒起来)
+ `has_dynamic_offset: true` + 每帧一个 bind group。这个分支是它的基础,自身也值得留着
(每个元素少 4 次跨进程分配)。

## 结构性修法:线性消失了

同一分支的第二个提交:整帧一个 uniform buffer(槽距取
`min_uniform_buffer_offset_alignment`,容量在录制前按 drawable 数的上界备好,因为中途扩容会
让已录进渲染通道的绑定失效),每个 draw 用 `queue.write_buffer` 写自己那一槽、按动态偏移
绑定;`BindGroupState` 去掉 uniform,只描述纹理绑定,于是连续的 draw 能复用同一个
bind group。

**每帧创建的对象不再随元素个数变化:**

| 每帧创建 | rects=0 | rects=200 |
| --- | --- | --- |
| buffer | 2.0 | **2.0** |
| bindGroup | 2.0 | **2.0** |
| sampler | 0 | 0 |
| texture view | 3.0 | 3.0 |

| 矩形数 | 修前 | 修后 | WebGL 参照 |
| --- | --- | --- | --- |
| 200 | 8.1fps / 103.06ms | **144.0fps / 3.16ms** | 144fps / 0.41ms |
| 1000 | 2.0fps / 434.46ms | **108.4fps / 8.09ms** | 144fps / 1.02ms |
| 5000 | — | **108.8fps / 8.02ms** | 144fps / 1.25ms |

1000 个矩形从 2.0fps 到 108.4fps,**54 倍**;1000 与 5000 的开销相同(8ms),线性没了。
距 WebGL 仍有约 6 倍,但已是同一量级。

**本项目的 3D 页,四轮下来:39 → 108.7 → 134.0fps**(帧率达到同机 WebGPU 天花板的
93%;GPU 每帧 6.34ms 对天花板的 1.63ms,仍是 3.9 倍,超出 `frame-rate.spec` 3 倍的上限)。
下面各轮的中间值:

**第二轮结束时是 108.7fps**(帧率达到同机 WebGPU 天花板的 75%,`frame-rate.spec`
的帧率断言因此转绿;GPU 每帧仍是天花板的 4.9 倍,超出 3 倍的上限,那是剩下的功课)。
画面经截图核对无误。动态偏移写错会渲染出垃圾而帧率照样好看,只看数字发现不了。

### 剩下的 144 → 110 是 bevy,不是缺陷

| | 帧率 | GPU 每帧 | 主线程 |
| --- | --- | --- | --- |
| `?tab=2`(带 bevy) | 113.5fps | 7.57ms | 54% |
| **`?tab=2&bevy=off`** | **144.1fps** | **4.64ms** | 32% |

修复前 `?bevy=off` 是 58fps。**现在 Slint 的界面单独跑满 144Hz。** 3D 页那 30fps 的差距是
bevy 每帧真的在画一个 3D 场景:约 2.9ms 的 GPU 进程时间,主线程占用从 32% 涨到 54%。
要把 3D 页也推到 144,该减的是 bevy 那边的每帧开销(draw call 数、场景复杂度、渲染
分辨率),属于普通的 3D 优化,与本文这个缺陷不是一类问题。

### 再压一轮:uniform 改成每 command 上传一次

GL 后端的 `set_uniforms` 调在 drawable 循环**外面**(`opengl.rs:275`),每个 command 一次;
wgpu 版的 `update_renderpass` 在循环**里面**,于是每个 drawable 都上传一次,尽管那里的
`params` 是循环不变量。照搬 GL 的结构即可,用这个文件里已有的单槽比较惯用法
(bind group 本来就是这么做的),不必动任何调用点,五处调用一并覆盖。

| 矩形数 | 每 draw 上传 | 每 command 上传 |
| --- | --- | --- |
| 100 | 144.0fps / 3.04ms | 144.0fps / 3.17ms |
| 300 | 144.1fps / 4.46ms | 144.1fps / 4.00ms |
| 600 | 135.9fps / 6.26ms | **143.8fps / 5.67ms** |
| 900 | 98.9fps / 9.26ms | **124.6fps / 6.79ms** |

每个元素 7.8µs → **4.5µs**(GL 是 0.25µs,还差约 18 倍)。

一处推断被测量纠正:原以为"N 个独立矩形就是 N 个 command,照搬无用"。实际上 femtovg
画抗锯齿边缘时会为同一个矩形额外发一次描边,两次 draw 共用同一份 `params`,上传正好减半。

### 再压一轮:整帧攒一次上传

每个 command 仍要一条 `write_buffer`(wire 命令 + 224 字节载荷)。改成录制时把各槽攒进
CPU 侧的 `Vec`(补齐到对齐槽距),录完一次性上传。上传发生在录制之后、调用方提交之前,
`write_buffer` 在队列上排在那次提交前面,所以数据到位。GL 那边没有对应物,它压根
没有 buffer,所以这一步是新机制,不是照搬。

三轮下来,每元素成本(取全在屏内的点算斜率):

| | 100 | 300 | 600 | 斜率 |
| --- | --- | --- | --- | --- |
| 每 draw 上传 | 3.04ms | 4.46ms | 6.26ms | 6.4µs/元素 |
| 每 command 上传 | 3.17ms | 4.00ms | 5.67ms | 5.0µs/元素 |
| **整帧一次上传** | **3.07ms** | **3.43ms** | **4.92ms** | **3.7µs/元素** |

600 个元素现在跑满 144fps(最初这一档是 135.9)。

**与 GL 的对比要按同样口径重测。** 此前"GL 每元素 0.25µs"是拿 5000 个矩形的数除出来的,
而其中只有约 640 个真的画了,与 900/1500 是同一个错。用 100/300/600 重测 GL:

| | 100 | 300 | 600 | 斜率 | 空载(每帧固定) |
| --- | --- | --- | --- | --- | --- |
| WebGL | 0.35ms | 0.41ms | 0.71ms | **0.72µs/元素** | ~0.28ms |
| WebGPU(现在) | 3.07ms | 3.43ms | 4.92ms | **3.7µs/元素** | ~2.9ms |

所以现状是:每元素约 **5 倍**于 GL,另外还有约 **2.6ms/帧的固定开销**是 GL 没有的。
两者在 600 元素处都跑满 144fps,对本项目的规模已经不构成问题。

注意这个指标量的是**浏览器里提交命令的 CPU 开销**,不是 GPU 的渲染能力。Chrome 的 WebGL
命令缓冲是多年打磨过的实现,而 WebGPU 每次调用要过 Dawn 的校验;femtovg 这边同样是
GL 后端成熟、wgpu 后端年轻。所以这不是"WebGPU 比 WebGL 慢",是这条路径上的实现差距。
WebGPU 的价值在别处:本项目要的 bevy 纹理共享,只有它做得到。

### 第四轮:顶点缓冲常驻 + 跳过冗余状态

先测了空载(`?rects=0`)的构成:每帧 **2.11ms**,其中 `HandleDawnCommands` 独占 0.71ms、
`Queue::Submit` 独占 0.61ms、`PutChanged` 0.51ms,而 `vkQueueSubmit` 只有 **0.06ms**。
每帧只有 2 次提交、画一个矩形,开销却全在 Dawn 的 CPU 侧。

两处照搬 GL:

- **顶点缓冲常驻**。原先每帧一次 `create_buffer_init`(整帧顶点数据走 alloc→map→copy→
  unmap),改成持久 buffer + `write_buffer` + 按 2 的幂增长,即 `opengl.rs` 的
  `buffer_data_u8_slice` 的做法。
- **跳过冗余的状态设置**。`set_pipeline` / `set_stencil_reference` / `set_bind_group`
  原先每个 draw 无条件调,而 GL 的 `select_main_program` 只在程序真的换了才重绑。

**这一步有个必须堵的正确性隐患**:渲染通道在切换渲染目标时会重建,新通道的状态是重置的。
所以 builder 给通道编了号,换号就把状态缓存全部作废。否则会跳掉必要的 `set_*`,
而且只在用到离屏层的界面上才暴露。

| 矩形数 | 第三轮 | **第四轮** | WebGL |
| --- | --- | --- | --- |
| 0 | ~2.9ms | **2.11ms** | ~0.28ms |
| 100 | 3.07ms | **2.45ms** | 0.35ms |
| 300 | 3.43ms | **2.12ms** | 0.41ms |
| 600 | 4.92ms | **3.01ms** | 0.71ms |

每元素 3.7µs → **约 1.2µs**,与 GL 的 0.72µs 基本同一量级;相对最初的 ~500µs,约 **400 倍**。
剩下的差距主要在**空载**:2.11ms 对 GL 的 0.28ms,而它既不是光栅化也不是命令记录。

**量这个基准的注意事项**:复现页的格子从 y=240 起、30px 一行、每行 40 个,视口
1280×720 只装得下约 **640** 个,再多的会被裁掉不画。900 与 1500 因此给出相同的数,
算斜率只能用 ≤600 的点。

另外此前"1000 与 5000 个矩形都卡在 8ms 说明存在每 draw 的固定开销"是**错的**。那是复现页
的裁剪假象:视口 1280×720,格子从 y=240 起、30px 一行,只有约 640 个在屏内。换成
100/300/600/900 之后线性一直都在。

另外每帧重建 10 条渲染管线的老问题仍在(`wgpu.rs` 的 `retain` 每次 `render()` 都扫,
见 `frame-rate.md`),现在值 0.11ms/帧。记着,但不值得为它单开一轮。

## 复现与测量工具

- `test/e2e/probes/minimal-repro.spec.ts`:跑最小复现并出数
- `test/e2e/probes/gpu-alloc.spec.ts`:数每帧创建了多少 GPU 对象
- 口径与三条环境约束见 [`test/e2e/README.md`](../../test/e2e/README.md)
