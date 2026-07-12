# Slint 的视觉效果上限:液态玻璃、动效与自定义 shader

调研日期:2026-07-12,基于 Slint 1.17(本仓库 Cargo.lock 实际解析版本)。

起因是一个常见误解:**"Slint 轻量,所以做不了高级 UI 效果"**。结论恰恰相反 ——
Slint 的效果上限是 **GPU 本身**,比 DOM/JS/CSS 那套高得多。真正受限的只有一处,
而且是可以绕过去的。

---

## 一、先把结论说清楚

> **Slint 做不到的不是"复杂效果",而是"跨越 Slint 渲染树的后处理"。**
>
> 凡是能在一个独立 GPU 层里自洽画完的效果,Slint 都能承载(自定义 WGSL shader、
> 零拷贝纹理共享,写什么都行);
> 凡是需要**读回 Slint 自己画出来的像素**的效果(典型:backdrop blur / 背景模糊折射),
> 今天就是不行。

液态玻璃(Liquid Glass)恰好横跨这两边,所以要拆开看。

---

## 二、Slint 1.17 内建能做什么(纯 .slint,不写一行 shader)

| 能力 | 支持 | 备注 |
|---|---|---|
| 渐变 | ✅ `@linear-gradient` / `@radial-gradient` / `@conic-gradient` | 1.17 起 radial/conic 支持 `at <x> <y>` 与半径 |
| 外阴影 | ✅ `drop-shadow-{color,blur,offset-x,offset-y,spread}` | `spread` 为 1.17 新增 |
| **内阴影** | ✅ `inner-shadow-{color,blur,offset-x,offset-y,spread}` | 1.17 新增,**仅 Skia 后端** |
| 圆角 | ✅ 四角独立 `border-*-radius` | |
| 透明度 / 旋转 / 裁剪 | ✅ `opacity` / `rotation-*` / `clip` | 软件渲染器不支持旋转、缩放、drop-shadow |
| 矢量路径 | ✅ `Path { commands: "..." }` | SVG path 语法,可 stroke / fill |
| 属性动画 | ✅ `animate <prop> { duration; easing; }`、`states` / `transitions` | 1.17 新增 `enabled` 开关 |
| 元素模糊 / 背景模糊 | ❌ | 见第三节 |
| 自定义 shader(声明式) | ❌ | 见第三节 |

**"看起来像玻璃"的那七成,纯 .slint 今天就能做,连 shader 都不用:**
半透明底色 + `inner-shadow`(给出玻璃厚度)+ `drop-shadow-spread`(悬浮感)+
`@conic-gradient`(扫边高光)+ 大圆角 + `animate` 让高光跟随指针移动。

> 本仓库现状提醒:`crates/ui/slint/app.slint` 里**一个渐变、一个 opacity 都没用过**,
> 内建能力完全没榨。另外 desktop 当前跑 femtovg,要用 `inner-shadow` 得切 `renderer-skia`。

---

## 三、缺的那一块:backdrop blur 与声明式 shader

两个入口至今都没开,已逐个核实上游状态:

- **backdrop blur** — [slint#2066](https://github.com/slint-ui/slint/issues/2066)
  "Add first-class support for blurring what's underneath a Rectangle"。
  2023-01 由 tronical(Slint 核心作者)本人开,**至今 open、无 milestone、无 PR**。

- **声明式自定义 shader** — [slint#10887](https://github.com/slint-ui/slint/issues/10887)(2026-02)。
  两次实现尝试均已关闭:
  - [PR #10874](https://github.com/slint-ui/slint/pull/10874) —— 语法为
    `Rectangle { background: fragment_shader("@fragment fn fs_main(...)"); }`(femtovg+wgpu / WGSL),
    2026-03-30 关闭,设计需重做。
  - [PR #11191](https://github.com/slint-ui/slint/pull/11191) —— wgpu-based custom shader,
    tronical 于 2026-05-22 关闭:*"Let's close this until we resume work on it."* 纹理导入部分未实现。

### 技术根因:拿不到"刚画完的那一帧"

`window().set_rendering_notifier()` 的回调里,`GraphicsAPI::WGPU29` 只暴露三个字段:

```rust
GraphicsAPI::WGPU29 { instance, device, queue }
```

**没有 surface texture,没有 command encoder。** 所以即便你在 `AfterRendering` 阶段拿到了
Slint 的 device,也无法把 Slint 刚渲染出的那一帧当作 shader 的输入纹理来做后处理。
这就是背景模糊做不了的全部原因 —— 不是性能问题,不是设计取向,是 API 没暴露。

---

## 四、能写 shader,但要看写在哪一层

### 路径 A:wgpu 离屏纹理 → `slint::Image`(**本仓库已在用,推荐**)

自己建 wgpu pipeline → 渲染到离屏纹理 → `slint::Image::try_from(texture)` → `.slint` 里当
`Image` 显示。**fragment shader 随便写(WGSL),无任何限制。**

本仓库 `crates/render3d` 已把最难的三件事解决:

- `render3d/src/lib.rs:139-149` —— `BackendSelector::new().require_wgpu_29(WGPUConfiguration::Manual { .. })`,
  **必须在建窗口之前**,让 Slint 与自己的渲染共用同一个 `wgpu::Device`(Slint 只能采样自己 device 上的纹理);
- `render3d/src/lib.rs:257-279` —— `Image::try_from(tex)`,纹理需 `Rgba8Unorm` +
  `Rgba8UnormSrgb` view,usage 含 `RENDER_ATTACHMENT | TEXTURE_BINDING`;
- `crates/ui/src/lib.rs:67-132` —— `run_with_renderer()` 的 seam:`Timer` 每帧回调 →
  `set_scene_3d(frame)` → `window().request_redraw()`(不请求重绘,Slint 的惰性渲染会冻住这块)。

**局限:这块内容对 Slint 是一张不透明位图** —— 它拿不到背后 Slint 画的按钮和文字,
反过来 Slint 也不参与它的内部合成。

### 路径 B:rendering notifier underlay / overlay

在 `BeforeRendering` / `AfterRendering` 里直接对窗口 framebuffer 发 draw call。
官方例子:[opengl_underlay](https://github.com/slint-ui/slint/tree/master/examples/opengl_underlay)。
受第三节所述的字段限制,**只能"在 Slint 之下/之上另画一层",不能"对 Slint 画的东西做后处理"**。

### 路径 C:窗口级模糊(平台 API)

macOS `NSVisualEffectView`、Windows `SetWindowCompositionAttribute(ACCENT_ENABLE_BLURBEHIND)`、
Wayland blur protocol。**只能模糊窗口背后的桌面,不能模糊应用内自己画的内容。**

---

## 五、液态玻璃的可行架构:把顺序翻过来(**本仓库已落地**)

既然读不回 Slint 的像素,那就**别让玻璃背后有 Slint 的像素**:

> **会被玻璃模糊的那层背景,整个交给 wgpu 自己画;模糊 + 折射 + 高光在自己的 WGSL
> fragment shader 里一次做完;合成结果作为一张 `Image` 交给 Slint,Slint 只在这张图
> 上面画真正的控件。**

代价说清楚:**玻璃后面不能有 Slint 控件**(模糊不了 Slint 画的按钮和文字),
只能模糊你自己在 GPU 层画的东西(壁纸 / 3D 场景 / 粒子 / 视频)。

对"玻璃工具栏浮在内容之上"这类真实需求,这个代价基本是白送的 —— 玻璃背后本来就是内容层,
而内容层交给 GPU 画反而更强。**只有"玻璃模糊玻璃"、"玻璃盖在列表控件上"这类嵌套场景才真正被挡住。**

### 落地:3D 页的热调工具条

- `crates/render3d/src/glass.wgsl` —— 圆角矩形 SDF + 32 点 Vogel 螺旋圆盘模糊 +
  沿 SDF 梯度的边缘折射 + 淡染。玻璃之外原样透传(实测与 bevy 直出逐字节相同)。
- `crates/render3d/src/glass.rs` —— 后处理 pass:在 bevy 产出的离屏纹理上跑一遍,
  写进另一张同尺寸纹理,再由 Slint 导入。
- 几何量的**唯一真相**在 `app.slint` 的 `glass-*` 属性(逻辑像素),Rust 乘窗口缩放系数换成
  物理像素传下去。别在 Rust 里重抄那些留白常量 —— 改了留白会静默错位。
- 分工:**模糊 / 折射 / 淡染归 shader;边框亮线、内侧厚度、悬浮阴影、指针高光仍归 Slint 的
  `GlassCard`**(`backdrop: true` 时它的淡白底让开,免得白色叠两遍)。

实测:同一个物体横跨工具条边界,**玻璃内的高频能量比紧邻的玻璃外低 44%** —— 是真的背景模糊。

对照组就在同一个 app 里:Home / Server 页的玻璃**没有**这一层(背后是 Slint 自己画的极光),
只能靠半透明透色,不会模糊。两者并排,就是这篇文档整个论点的实物证明。

---

## 六、为什么说上限比 DOM/JS/CSS 高

直觉上 Web 更强 —— 毕竟 CSS 有 `backdrop-filter`,Slint 没有。但**看的是天花板,不是地板**:

| | Web(DOM/JS/CSS) | Slint |
|---|---|---|
| 开箱的背景模糊 | ✅ `backdrop-filter`(Slint 输在这) | ❌ |
| 突破内建效果的逃生舱 | `<canvas>` + WebGL/WebGPU | wgpu 离屏纹理 + `Image::try_from` |
| **逃生舱与 UI 层能互相后处理吗** | **❌ 不能**(canvas 与 DOM 之间同样无法互相合成) | **❌ 不能**(同构的限制) |
| 逃生舱的代价 | JS 桥、主线程、合成器往返、纹理上传 | **同进程、同一个 `wgpu::Device`、原生 Rust、零拷贝** |
| 天花板由谁决定 | **CSS/SVG 规范**(且 `backdrop-filter` + SVG filter 的组合只有 Chromium 支持,Safari/Firefox 不支持) | **GPU 本身** |

两条关键事实:

1. **结构性限制是同构的。** Web 上 `<canvas>` 里的东西和 DOM 里的东西同样无法互相做后处理 ——
   你没法用 CSS `backdrop-filter` 去模糊 canvas *内部*的分层,也没法让 canvas 采样 DOM 渲染的结果。
   Slint 的"wgpu 层 ↔ Slint 树"边界,和 Web 的"canvas ↔ DOM"边界是同一回事。
   **Slint 只是少了 `backdrop-filter` 这一个内建便利,而不是少了一整类能力。**

2. **逃生舱的质量差一个数量级。** Web 要突破 CSS 就得进 canvas,而那意味着 JS 调用开销、
   与合成器的往返、以及 DOM 与 canvas 两套坐标/事件系统的割裂。
   Slint 这边:**同一个进程、同一个 `wgpu::Device`、原生 WGSL、纹理零拷贝**,
   写的是真正的 GPU 代码,上限就是显卡的上限。本仓库 `render3d` 把一整个 bevy 渲染管线塞进
   Slint 的一个 `Image` 属性里,全程没有一次像素拷贝 —— 这在 Web 上是做不到的。

所以正确的说法是:
**Slint 的"轻量"是运行时轻量(二进制小、内存低、启动快),不是能力轻量。
它缺的是几个内建的便利语法,不是缺表达力。**

---

## 七、已知阻塞:web 端全线不可用

`renderer-femtovg-wgpu` 在 wasm 上被 `#[cfg]` 整个排除,
[slint#11580](https://github.com/slint-ui/slint/pull/11580)(milestone 1.18,draft,未合)落地前,
web 无法导入 wgpu 纹理 —— 即上述路径 A 在浏览器里不存在。

详见 [`docs/adr/0005-wgpu-device-as-shared-base.md`](../adr/0005-wgpu-device-as-shared-base.md),
架构护栏在 `xtask/src/boundaries.rs`(禁止 `app-web` / `app-ios` 依赖 `render3d` / `bevy` / `wgpu`)。

---

## 参考

- [Slint Rectangle 元素文档](https://docs.slint.dev/latest/docs/slint/reference/elements/rectangle/)
- [Slint CHANGELOG](https://github.com/slint-ui/slint/blob/master/CHANGELOG.md)
- [`GraphicsAPI` enum](https://docs.slint.dev/latest/docs/rust/slint/enum.GraphicsAPI)
- [`slint::wgpu_29` 模块](https://docs.slint.dev/latest/docs/rust/slint/wgpu_29/)
- [官方 opengl_underlay 例子](https://github.com/slint-ui/slint/tree/master/examples/opengl_underlay)
