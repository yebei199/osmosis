//! 液态玻璃后处理:在 bevy 画好的画面上,对一个圆角矩形区域做模糊 + 边缘折射。
//!
//! 存在的理由:**Slint 没有 backdrop blur,也拿不到自己渲染的像素**(`GraphicsAPI::WGPU29`
//! 只给 instance/device/queue,没有 surface texture)。但玻璃背后这块背景是我们自己在 GPU 上
//! 画的,所以我们能采样它 —— 这就是 `docs/slint/visual-effects-and-shaders.md` 第五节说的
//! "把顺序翻过来":背景层归 GPU,控件层归 Slint,玻璃在两者之间的 GPU 层里合成。
//!
//! 代价同样写在那篇文档里:**玻璃背后不能有 Slint 控件** —— 这里模糊的只是 bevy 的画面。
//!
//! ## 为什么长在 bevy 的渲染管线里,而不是自己起一个 pass
//!
//! 早先的实现自己建一张输出纹理、自己 `queue.submit`,再把那张纹理交给 Slint 采样。
//! 安卓上拖动时会闪黑:那张纹理由 bevy 之外的通道写,又被 Slint 用 wgpu 看不见的命令读,
//! 两侧没有屏障。实测把这个 pass 关掉就是 0 次闪黑,而缩小撞车窗口的办法(改 `LoadOp`)
//! 只能把频率压到三分之一。整个排查见 `docs/wasm/native-regression-2026-07-19.md`。
//!
//! 现在它是 bevy 的一个 [`FullscreenMaterial`],在 bevy 自己的 ping-pong 纹理上做,
//! 最终结果由 bevy 写进那张共享纹理。链上只剩一张纹理、只有一个写入方。

use bevy::asset::embedded_asset;
use bevy::core_pipeline::fullscreen_material::{
    FullscreenMaterial, FullscreenMaterialPlugin,
};
use bevy::math::{Vec2, Vec4};
use bevy::prelude::*;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_resource::ShaderType;
use bevy::shader::ShaderRef;

/// 模糊半径(物理像素)。够糊出磨砂感,又不至于把工具条背后糊成一片死白。
const BLUR_PX: f32 = 14.0;

/// 要做成玻璃的那个圆角矩形,**物理像素**,坐标系与离屏纹理一致(左上角为原点)。
///
/// 由 UI 侧给出(`app.slint` 里工具条的几何量 × 窗口缩放系数),不在这里重复那些常量 ——
/// 否则 .slint 改了留白,shader 这边会静默错位。
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct GlassRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub radius: f32,
}

impl GlassRect {
    /// 宽高有一个不为正就没什么可做的(3D 页未激活、或面板还没量出尺寸)。
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

/// 挂在相机上的玻璃参数。字段顺序与布局必须与 `glass.wgsl` 的 `struct Params` 一致。
#[derive(
    Component,
    ExtractComponent,
    Clone,
    Copy,
    ShaderType,
    Default,
)]
pub struct GlassMaterial {
    /// 玻璃矩形:xy = 左上角,zw = 宽高,物理像素。
    pub rect: Vec4,
    /// 目标纹理尺寸,物理像素。
    pub tex_size: Vec2,
    pub radius: f32,
    pub blur: f32,
}

impl GlassMaterial {
    /// 从 UI 给的矩形与当前纹理尺寸组装。
    pub fn new(rect: GlassRect, tex: (u32, u32)) -> Self {
        Self {
            rect: Vec4::new(rect.x, rect.y, rect.w, rect.h),
            tex_size: Vec2::new(tex.0 as f32, tex.1 as f32),
            radius: rect.radius,
            blur: BLUR_PX,
        }
    }
}

impl FullscreenMaterial for GlassMaterial {
    fn fragment_shader() -> ShaderRef {
        "embedded://render3d/glass.wgsl".into()
    }
}

/// 把玻璃效果注册进 bevy 的渲染管线。
pub struct GlassPlugin;

impl Plugin for GlassPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "glass.wgsl");
        app.add_plugins(FullscreenMaterialPlugin::<
            GlassMaterial,
        >::default());
    }
}
