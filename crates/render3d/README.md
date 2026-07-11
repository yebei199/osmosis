# render3d

3D 桥:用 **bevy**(git main,含 `bsn!` 等新特性)在**共享的** wgpu-29 device 上
离屏渲染,产出一张 `slint::Image` 交给 UI 层合成。

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

## 依赖钉扎

bevy 用 git `rev` 钉死(见根 `Cargo.toml`)。main 是移动靶,不钉会随时间漂到连
编译都不稳;`bsn!` 只在 main,故不能用 0.19 稳定版。

## 运行

需要 vulkan 运行期库,用仓库根的 `render3d.nix`(在 `slint.nix` 基础上加 `vulkan-loader`):

```
nix-shell render3d.nix --run "cargo run -p app-desktop --features bevy-3d"
```
