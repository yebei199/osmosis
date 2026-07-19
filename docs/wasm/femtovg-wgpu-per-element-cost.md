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
(代码见 `apps/web/src/lib.rs` 的 `repro` 入口)。`?rects=N` 再铺 N 个**静止**的小矩形 ——
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
`Display::DrawAndSwap`(真正呈现出去的帧),**不是 rAF 频率** —— 两者不是一回事。

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

## 解法

1. **退回 WebGL 渲染器**(马上可用)。代价是失去 3D —— 纹理共享必须要 WebGPU,
   目前只能二选一。
2. **缓存不变的对象**:2 个 sampler 与 2 个空纹理 view 挂成 `WGPURenderer` 的字段,
   6→2。改动小、不碰绑定布局、无 API 变化。
3. **结构性修法**:整帧一个大 uniform buffer + `has_dynamic_offset: true`,每帧只一个
   bind group,6→0。要改绑定布局与着色器绑定点,并把 uniform 按 256 对齐攒起来。

## 复现与测量工具

- `test/e2e/probes/minimal-repro.spec.ts` —— 跑最小复现并出数
- `test/e2e/probes/gpu-alloc.spec.ts` —— 数每帧创建了多少 GPU 对象
- 口径与三条环境约束见 [`test/e2e/README.md`](../../test/e2e/README.md)
