# wgpu-29 device 作全端统一基座

本应用 wgpu 优先:每个端的 Slint 都跑在一个 **wgpu-29 device** 上,各端保留自己最合适的
renderer 引擎 —— mobile 是 skia-on-wgpu,desktop / web 是 femtovg-on-wgpu。3D 桥(bevy)
在同一个 device 上离屏渲染,产出的纹理才能被 Slint 采样(见 `crates/render3d`)。

**现状(2026-07-11)**:web 端已证实被上游硬阻塞,见下方「web 阻塞」。desktop / android 走
原生 wgpu(非 wasm),不受此限制。

## 为什么

让 bevy 3D 能在**任意端**嵌入,前提是 Slint 与 bevy 共享同一 wgpu device(纹理类型同源)。
与其「只有桌面/安卓能上 3D、其余端 graceful 隐藏」,不如把 wgpu device 作为全端统一基座:
3D 面板在哪个端都能开(仍是默认关的 `bevy-3d` feature),不必为「哪些端有 3D」维护分叉。

真正会「产物体积爆炸」的是 **bevy**(wasm 产物几十 MB),**不是 wgpu** —— wgpu 只是 Slint 的
渲染后端,把 femtovg-GL 换成 femtovg-wgpu 的体积增量温和。此前 `xtask boundaries` 把
`wgpu` 当作 bevy 的代理词一并禁进 web/ios,是过度收紧:现已放开 web 的 wgpu,只保留对
bevy/render3d 的禁令。

## 对 ADR-0003 的定向反转

ADR-0003 的设计价值观是「平台专属依赖别外溢到不需要它的端」。本 ADR 就 **wgpu 这一项**定向
反转它:wgpu 不再被视作「专属依赖」,而是**全端共同基座**。其余依赖(wasm-bindgen、tokio 等)
的外溢约束不变,ADR-0003 仍然有效。

## 连带约束

- **desktop 去掉 `renderer-software` 软渲染兜底** → 运行期强制要 Vulkan。`slint.nix` 需并入
  vulkan-loader(此前只有 `render3d.nix` 有)。无 GPU / headless 机器不再能跑桌面窗口。
- **web 默认也需 WebGPU 浏览器**(femtovg-wgpu → WebGPU)。这是 dev 链路,没 WebGPU 直接
  panic 白屏,可接受,不做能力探测降级。—— **见下方「web 阻塞」,此条在当前 Slint 版本下
  连编译产物都不存在,不是运行期能力探测的问题。**
- **web 的 3D 构造是异步的**:wasm 主线程不能 `block_on`,故 `Scene::new_async().await`
  取 device 后再建窗口(桌面/安卓仍走同步 `Scene::new`)。这一层接线本身没问题,但下面
  卡的是更底层的东西,接线接得再对也绕不过去。

## web 阻塞:femtovg-wgpu 在 wasm 上当前不存在,不是运行时降级问题

`renderer-femtovg-wgpu` + wasm 这个组合,在 Slint 1.17(本仓库锁定版本,`Cargo.lock` 里
`slint 1.17.0`,截至写这条 2026-07-11 最新 release 是 1.17.1)下**编译期就没有实现**,
不是接线方式的问题:

```rust
// internal/backends/winit/renderer/femtovg.rs (Slint 1.17 系列)
#[cfg(all(feature = "renderer-femtovg-wgpu", not(target_family = "wasm")))]
pub struct WGPUFemtoVGRenderer { ... }
```

`WGPUFemtoVGRenderer` 整个结构体被 `not(target_family = "wasm")` 排除 —— 在 wasm target 上
这个类型根本不存在。所以:

1. **Slint 自己的 femtovg-wgpu 在 web 上跑不起来**(与 bevy 无关,纯 2D 也一样)。
2. **外部 wgpu 纹理导入**(`crates/render3d` 依赖的 `require_wgpu_29(WGPUConfiguration::Manual{..})`
   + `Image::try_from(wgpu::Texture)`)因此**无处挂载** —— API 类型本身(`wgpu_29.rs` 里
   `impl TryFrom<wgpu_29::Texture> for Image`)并没有 wasm 专门排除,但没有 wasm 上能用的
   wgpu 渲染器,这个 API 就没有承接的窗口 surface。

结论:「web 上 wgpu + bevy 3D」在 Slint 1.17 下不可能,和本仓库 `Scene::new_async` 的
async 接线方式无关 —— 就算接线完全正确,底层类型都不存在。

### 上游 issue / PR

- **[slint-ui/slint#9685](https://github.com/slint-ui/slint/issues/9685)**(open,1.14 起报):
  维护者 tronical 确认「a build with Slint and wgpu targeting wasm32-unknown-unknown isn't
  implemented yet」。
- **[slint-ui/slint#11580](https://github.com/slint-ui/slint/pull/11580)**
  "Add support for using the FemtoVG renderer with wgpu in WASM builds"(tronical,
  2026-04-30 提交,Closes #9685)。
  - 状态:**OPEN,未合并**,milestone `1.18`(无 due date,尚未发布)。
  - 作者原话:「This isn't the default yet as there are some bugs in slintpad demo
    rendering — so this PR just permits an opt-in」—— 即便合并,也是 opt-in、非默认,
    且作者自认还有渲染 bug。
  - 已核对 diff:`WGPUConfiguration::Manual`(外部传入 instance/adapter/device/queue,
    即 `crates/render3d` 用的共享 device 路径)分支本身是纯同步代码,这条 PR **没有改动**
    它 —— 意味着 #11580 一旦落地,本仓库「共享 wgpu device 给 bevy 纹理」的用法大概率能
    直接接上,不需要为 wasm 再单独适配 texture-import 那一层。真正要等的是 wasm 上
    femtovg-wgpu 渲染器本身从无到有。

#### 为什么之前整块排除、现在又能放开:根子在「谁的 future 会真异步」

旧代码把 wgpu 的 instance/adapter/device 初始化(`request_adapter` / `request_device`,
本身是 `async fn`)包一层 `poll_once` 当同步函数用,注释写得很直白:「wgpu uses async
here, but the returned future is ready on first poll on all platforms **except WASM**」。
这是因为:

- **原生后端**(Vulkan/Metal/DX12/GL):`wgpu-hal` 在 `request_adapter`/`request_device`
  内部就是同步完成的,套壳成 `async fn` 只是为了跟 WebGPU 的接口对齐 —— 第一次 `poll`
  就 `Ready`,`poll_once` 拿到结果毫无问题。
- **wasm + WebGPU**:`request_adapter`/`request_device` 落到 `navigator.gpu.requestAdapter()`
  /`requestDevice()`,这是真正的 **JS Promise** —— 要等浏览器把 GPU 进程握手完,至少要经过
  一次微任务(microtask)甚至多次事件循环 tick 才 resolve。`poll_once` 只戳一下就走人,
  在 wasm 上必然拿到 `Poll::Pending`,触发 `.expect("internal error: wgpu setup is not
  expected to be async")` panic。旧维护者的应对是不解这个坑,而是**直接把整个
  `WGPUFemtoVGRenderer` 结构体连同 impl 一起 `cfg` 掉**,wasm 上编译期当它不存在 ——
  这就是我在上一版 ADR 里引的那行 `not(target_family = "wasm")`。

  这也解释了为什么此前「纯 2D femtovg-wgpu on web」同样不成立,和 bevy/纹理导入完全
  无关:根子是**类型压根没编出来**,不是运行时能力探测或降级问题。

- **PR #11580 怎么解的**:把 `init_instance_adapter_device_queue_surface` 拆成两层——
  底层 `async_init_instance_adapter_device_queue_surface` 是真正的 `async fn`(内部
  `.await` 真实的 adapter/device future);原来的同步版本降格成一层薄壳,只在**非 wasm**
  下用 `poll_once` 驱动它(因为只有非 wasm 才保证首次 poll 就绪)。
  然后在 `WGPUFemtoVGRenderer::show()`(winit 建窗口的回调,**这个回调本身必须保持
  同步**——`ActiveEventLoop` 的 `create_window` 系列回调不是 async fn,没法在里面
  `.await`)里按 target 分叉:非 wasm 直接调同步壳拿到 device 就地配好 surface;
  wasm 上用 `context.spawn_local(wgpu_init_future)` 把「等 Promise resolve → 配置
  surface → 请求重绘」整体丢进一个异步任务,`show()` 自己立刻返回已创建好的
  (但还没能画东西的)`winit::window::Window`。等 spawn 出去的任务真正拿到 device 后,
  再回调 `finalize_wgpu_init` 补上 surface 配置并 `request_redraw()`。

  这和本仓库 `apps/web` 此前的写法(`wasm_bindgen_futures::spawn_local` 包一层
  `render3d::Scene::new_async().await` 再建窗口)是**同一个思路**:wasm 主线程不能
  `block_on`,只能把「真异步的那一小段」丢给事件循环自己转,建窗口这类必须同步的调用
  留在原地不动。只是 #11580 把这个模式**下沉进了 Slint 内部**,让 Slint 自己按需异步,
  而不是要求调用方（应用层）自己接管全部异步初始化。

#### 合并之后能不能用 wgpu?—— 分两种用法说

- **Slint 自己建 device**(`WGPUConfiguration::Automatic` 或不传,即「纯用 Slint 的
  femtovg-wgpu 渲染 2D」):**可以**,但只是 opt-in、非默认(需要显式选中这条路径,
  作者原话见上)。行为上是「先探测 WebGPU,没有就自动落到 wgpu 的 WebGL 后端」——
  代码里特意保留 `wgpu::Backends::empty()`(不排除 GL)而不是像原生分支那样排除
  `Backends::GL`,注释写明「we *want* the GL backend to be available」,就是为了
  在 headless Chromium / 没开 WebGPU flag 的浏览器上兜底,不至于建 device 直接失败。
- **本仓库这种「外部共享 device」用法**(`WGPUConfiguration::Manual`,即
  `crates/render3d::Scene` 自己 `request_adapter`/`request_device` 后再喂给
  Slint):**也可以**,而且更轻——`Manual` 分支在 `async_init_instance_adapter_
  device_queue_surface` 里没有真正的 `.await`(instance/adapter/device/queue 都是
  调用方现成传入的),`spawn_local` 出去的那个 future 基本是下一个 microtask 就跑完,
  只多一次 tick 的延迟,不是真的要等浏览器的 GPU 握手。也就是说:**render3d 现在的
  `Scene::new_async()` 模式,在 #11580 合并后不需要重新设计**,只是外层「先 await 拿到
  device 再建窗口」这道手动接线,理论上可以退化成让 Slint 内部的 `spawn_local` 顺带
  处理——但保留应用层显式 `new_async().await` 依然合理,因为 bevy 那边的无头场景
  （`spawn_scene`、`RenderCreation::manual`)也需要同一份 device,不能只靠 Slint 内部
  拿到就完事。
- **仍未解的风险**(对应「slintpad demo rendering 的 bug」):`show()` 在 wasm 上把
  「建窗口」和「配好能画东西的 surface」拆成了两步、中间隔着至少一次 microtask —— 这段
  空档期窗口存在但画布是空的(白屏/上一帧残留),且如果 `async_init_...` 失败
  (两种 backend 都拿不到),代码路径是 `debug_log!` 一行日志后直接 `return`,**没有
  降级到 GL femtovg**,画面会永久空白而不是报错终止。真要接这条路径,仍需自己在
  应用层加超时/失败提示,不能假设「能建出 wgpu device」是必然成立的。

### 对本 ADR 的影响

- 「分端迁移」清单里原「1. web:… ← 本 ADR 落地时已做」**不成立**,已撤销相关代码
  (`apps/web` 的异步 3D 接线、`bevy-3d` feature 在 web 上的开启)。web 暂时退回纯 2D
  femtovg-wgpu 也一样跑不了 —— 见上,只能等上游或换回其他 renderer。
- `xtask boundaries.rs` 对 app-web 的 wgpu/bevy 禁令需要保留(不能像本 ADR 原计划那样放开),
  直到 #11580 合并且发布到 stable。
- 复查时机:关注 slint milestone `1.18` 发布,或 #11580 合并状态变化。

## 分端迁移(每端一个原子提交)

1. **web**:❌ **阻塞**,见上「web 阻塞」。不做 `renderer-femtovg-wgpu` + `bevy-3d`,
   待上游 slint#11580 合并发布后重新评估。
2. **desktop**:base 换 `renderer-femtovg-wgpu`、去 `renderer-software`,vulkan 并入 slint.nix
3. **android**:base 加 `unstable-wgpu-29`(skia 切 wgpu 路径)
4. **ios**:skia-on-wgpu —— **需先实测** backend-winit+Metal 是否支持;boundaries 里 app-ios
   的 wgpu 禁令等这步再放开

未落地的端在 `boundaries.rs` 里仍按旧约束守着,逐端放开。
