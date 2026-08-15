//! 3D 桥:用 **bevy** 在**共享的** wgpu-29 device 上离屏渲染,产出一张 [`slint::Image`]
//! 交给 UI 层合成。桌面 / android 入口硬依赖本 crate;web / ios 永不碰它 ——
//! 由 `xtask boundaries` 守住这条边界。
//!
//! 架构约束(见计划 `bevy-serialized-dove`):
//! - device 由本 crate 自建(Manual),同一套 instance/adapter/device/queue
//!   既注入 Slint 的 `require_wgpu_29`,也注入 bevy 的 `RenderCreation::manual`。
//! - bevy 主线程无头运行,禁 `bevy_winit`,由 Slint 的 `Timer` 每帧驱动 `app.update()`,
//!   绝不调 `App::run()` —— 事件循环永远归 Slint。
//! - bevy 与 Slint 共享同一 wgpu 大版本(现为 29),纹理类型才是同一个,才能被 Slint 采样。
//!
//! 每帧产出**两张**图:粒子场本身,以及一张只含「比标注卡更近」的片元的遮挡层
//! (见 [`spawn_occluder_camera`])。UI 侧把二者夹着卡片叠三层,卡片就被粒子
//! 逐像素挡住 —— 深度正确的 UI。被标注的物体是 `marker` 那枚绕轨道走的方块;
//! 点云当不了它,理由写在 [`CLOUD_ORIGIN`] 下面那段。
//!
//! 用法(见 `apps/desktop`):先 [`Scene::new`](Scene::new) —— 它顺带配好 Slint 的 wgpu 后端,
//! 必须在建窗口**之前**调 —— 再把 `move || scene.render_viz_frame()` 交给
//! `ui::run_with_renderers`。

mod aurorabtn;
mod navglass;
pub use aurorabtn::{
    AuroraBtnParams, AuroraBtnPass, AuroraBtnSlot,
};

pub use navglass::{NavGlassPass, NavParams};

mod warp;
pub use warp::{AUDIO_BYTES, WARP_SIDE, WarpPass};

mod cloud;
mod marker;
mod wall;
pub use wall::{
    WallCamera, WallCard, WallCover, WallFrame,
};

mod camera;
mod scene;
mod seam;

pub use scene::Scene;
pub use seam::{
    CoverUpdate, Pointer, SharedTexture, VizFrame,
};
