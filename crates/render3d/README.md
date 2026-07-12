# render3d

3D 桥:用 **bevy**(0.19 稳定版)在**共享的** wgpu-29 device 上离屏渲染,
产出一张 `slint::Image` 交给 UI 层合成。

## 为什么存在 / 边界

主体是 Slint 应用,本 crate 只负责「Slint 做不来的 3D 效果 / 小游戏」那一块。

- **只有桌面 / android 入口依赖它**,web / ios 永不碰 —— 由 `xtask boundaries` 守住。
  bevy/wgpu 因此不会污染到不该有它的端。
- `ui` 对 bevy / wgpu 一无所知:本 crate 把渲染结果作为 `slint::Image` 经
  `ui::run_with_renderer` 的 seam 交过去(见 `crates/ui`)。

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

## 依赖版本

bevy 用 0.19 稳定版(crates.io)。BSN(`bsn!` 宏 + 场景系统)已随 0.19 发布,本 crate
采用之;它藏在 `bevy_scene` feature 后(见根 `Cargo.toml`),只拖纯 Rust 序列化 crate、
无系统库。

## 运行

需要 vulkan 运行期库,用仓库根的 `render3d.nix`(在 `slint.nix` 基础上加 `vulkan-loader`):

```
nix-shell render3d.nix --run "cargo run -p app-desktop --features bevy-3d"
```
