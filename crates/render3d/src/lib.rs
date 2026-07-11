//! 3D 桥:用 bevy 在**共享的** wgpu-29 device 上离屏渲染,产出一张 [`slint::Image`]
//! 交给 UI 层合成。只有桌面 / android 入口依赖本 crate,web / ios 永不碰它 ——
//! 由 `xtask boundaries` 守住这条边界。
//!
//! 架构约束(见计划 `bevy-serialized-dove`):
//! - device 由本 crate 自建(Manual),同一套 instance/adapter/device/queue
//!   既注入 Slint 的 `require_wgpu_29`,也注入 bevy 的 `RenderCreation::manual`。
//! - bevy 主线程无头运行,禁 `bevy_winit`,由 Slint 的 `Timer` 每帧驱动 `app.update()`,
//!   绝不调 `App::run()` —— 事件循环永远归 Slint。
//! - bevy 与 Slint 共享同一个 wgpu 大版本(现为 29),纹理类型才是同一个,
//!   才能被 Slint 采样。任一方升级须先核对 wgpu 大版本。

// 首次编译门(已过):bevy main 全家 + slint 1.17 均编过,且 Cargo.lock 里只有
// 一份 wgpu(29.0.4)—— slint 的 wgpu-29 与 bevy 的 wgpu 被 cargo 统一为同一 crate。
// 因此下面这个别名指向的正是 bevy 也在用的那个 `wgpu::Texture`,纹理可跨库共享。
// 真正的编译期证明留给后续 `Image::try_from(bevy_texture)` 的实际调用。

/// bevy 与 slint 共享的 `wgpu::Texture` 类型别名。
///
/// render3d 不直接依赖 `wgpu` crate,统一经由 slint 的 `wgpu_29` 再导出拿到它 ——
/// 反正是同一份 crate。离屏纹理以此类型在 bevy 与 Slint 之间传递。
pub type SharedTexture = slint::wgpu_29::wgpu::Texture;
