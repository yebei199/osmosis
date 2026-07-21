# render3d

3D 桥:用 **bevy**(0.19 稳定版)在**共享的** wgpu-29 device 上离屏渲染,
产出两张 `slint::Image`(场景 + 遮挡层)交给 UI 层合成。

## 为什么存在 / 边界

主体是 Slint 应用,本 crate 只负责「Slint 做不来的 3D 效果 / 小游戏」那一块。

- **桌面 / android / web 入口按 `bevy-3d` feature 依赖它**,ios 永不碰;web / ios 的
  **默认**构建不拉它 —— 由 `xtask boundaries` 守住(默认 feature 集里无 3D)。
- **web 入口用 `Scene::new_async().await`**:浏览器的 WebGPU 初始化是真 Promise,
  wasm 主线程不能 `block_on`;原生端仍用同步 `Scene::new()`(同一 future 首次 poll 即就绪)。
- `ui` 对 bevy / wgpu 一无所知:本 crate 把渲染结果作为**两张** `slint::Image`
  (场景 + 遮挡层)经 `ui::run_with_renderer` 的 seam 交过去(见 `crates/ui`)。

## 关键架构约束(见计划 `bevy-serialized-dove`)

1. **共享 device**:instance/adapter/device/queue 由本 crate 自建(Manual),
   同一套既注入 Slint 的 `require_wgpu_29`,也注入 bevy 的 `RenderCreation::manual`。
   Slint 只能采样属于自己 device 的纹理,所以共享是硬性要求。
2. **事件循环归 Slint**:bevy 禁用 `bevy_winit`、无头运行,由 Slint 的 `Timer`
   每帧驱动 `app.update()`,绝不调 `App::run()`。
3. **wgpu 版本对齐**:bevy 与 slint 必须共享同一 wgpu 大版本(现为 29,Cargo.lock
   里实为单份 `wgpu 29.0.4`)。升级任一方前先核对,否则 `wgpu::Texture` 类型不兼容。

## 场景内容

两个可切换场景,均由 `bsn!`(BSN,next-gen 场景系统)声明式构造,挂在一个转盘根
`root` 下:

- **形状画廊**(`scene_id==0`):`count` 个内置图元(Cuboid/Sphere/Torus/Capsule/
  Cylinder/Cone 调色板循环)沿 XZ 环形均布。
- **实体阵列**(`scene_id==1`):`count` 个 Cuboid 排成近正方网格。

由 `SceneParams`(POD,镜像 `ui::SceneControls`,由 apps/* 在 seam 处翻译)驱动。
其中 `scene_id`/`count`/`color`/`spacing` 变化时脏检查 → despawn 子树 → bsn! 重建;
`yaw`/`pitch`/自转每帧只写 `root.Transform`(转盘)。参数来自 3D 页的 LineEdit 热调。

## 液态玻璃后处理(`glass.rs` + `glass.wgsl`)

bevy 画完那一帧后,再跑一个全屏 fragment shader:把热调工具条那块圆角矩形区域内的画面
**模糊 + 边缘折射 + 淡染**,区域之外原样透传,结果写进另一张同尺寸纹理再交给 Slint。

存在的理由:**Slint 没有 backdrop blur,也拿不到自己渲染的像素**(`GraphicsAPI::WGPU29`
只给 instance/device/queue,没有 surface texture)。但玻璃背后这块背景是我们自己在 GPU 上
画的,所以我们能采样它 —— 详见 `docs/slint/visual-effects-and-shaders.md` 第五节。

- 玻璃矩形的几何量由 UI 侧给出(`app.slint` 的 `glass-*` 属性 × 窗口缩放系数),
  **是唯一真相**;这里不重抄那些留白常量,否则 .slint 改了留白 shader 会静默错位。
- 分工:模糊/折射/淡染在 shader;边框、厚度、阴影、指针高光仍由 Slint 的 `GlassCard` 画在上面。
- 代价:**玻璃背后不能有 Slint 控件** —— 能模糊的只有 bevy 的画面。

## 深度正确的 UI(遮挡层)

Slint 的合成没有深度:3D 画面对它只是一张 `Image`,任何 Slint 控件都恒在其上。要让场景
里的物体挡住一张 Slint 卡片,把合成拆成三层 —— **场景 → 卡片 → 遮挡层**。遮挡层由
**第二台相机**画:与主相机同位、同投影、同色调映射,只差两处 —— 清除色透明,且深度缓冲
不清到远平面而是清到卡片所在的深度。于是它只剩「比卡片更近」的片元,其余透明,Slint 那边
只做寻常的 alpha 合成。UI 不必先渲进纹理,Slint 也不必懂深度。

- **门槛值**是锚点(转盘中心)的 NDC 深度,每帧由 `Camera::world_to_ndc` 算出。bevy 用
  反向 Z(1 是近平面、0 是远平面),深度测试是 `GreaterEqual`,「清到锚点深度」正好筛掉
  更远的片元。锚点跑出视锥时退回近平面 → 遮挡层为空、卡片完整可见:宁可少一个效果,也
  不能把整幅场景糊在卡片上。
- **逐片元,不是逐物体**:横跨锚点平面的物体会被平面切开。CPU 侧按物体排序做不到,而
  「UI 整层贴在 canvas 上」的方案在原理上就做不到 —— 这正是这条路线买到的东西。
- **代价是几何提交两遍**。8~64 个形状可忽略。要省的话:卡片隐藏时关掉这台相机的
  `is_active`,或改成采样深度纹理的一个全屏 pass(那需要自建渲染图节点与 WGSL,现在不值)。
- 遮挡层那张 Image 在 `app.slint` 里是**整块内容区**大小的(必须和场景那张逐像素对齐),
  用负偏移推回内容区原点,再由卡片那层 `clip` 裁到卡片范围。不裁的话,近处的物体会连
  热调工具条一起盖住 —— 工具条是界面外壳,不在场景里。

判断它有没有生效见 [`AGENTS.md`](../../AGENTS.md):**量卡片边框的像素,别看整幅图的观感**。

## 依赖版本

bevy 用 0.19 稳定版(crates.io)。BSN(`bsn!` 宏 + 场景系统)已随 0.19 发布,本 crate
采用之;它藏在 `bevy_scene` feature 后(见根 `Cargo.toml`),只拖纯 Rust 序列化 crate、
无系统库。

## 运行

需要 vulkan 运行期库,用仓库根的 `render3d.nix`(在 `slint.nix` 基础上加 `vulkan-loader`):

```
nix-shell render3d.nix --run "cargo run -p app-desktop --features bevy-3d"
```

## 相机随视口长宽比后撤

透视投影固定的是**垂直**视野,水平视野 = 垂直视野 × 长宽比。视口一竖(手机竖屏、桌面拖窄的
紧凑版式,长宽比可低到 0.26),横向排开的形状画廊就出画。

`pullback(aspect)` 给出后撤倍数(参考长宽比 / 当前长宽比,封顶 4 倍),相机的**位置向量整体
乘这个倍数** —— 而不是只退 z。只退 z 的话视线会越来越水平,转盘被看成侧视的一条扁线;整体缩放
则保住了俯视角。宽视口(长宽比 ≥ 1)返回 1,观感与改动前逐像素一致。见
[`docs/adr/0007`](../../docs/adr/0007-layout-mode-by-width-not-by-platform.md)。
