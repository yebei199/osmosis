//! 卡墙上「正在放的那一张」的闪卡材质。效果全在 `foil.wgsl` 里。
//!
//! 走 bevy 的材质管线而不是自起一条 wgpu pass(navglass 那种):卡片本来就
//! 是场景里的方片,位姿、深度排序、相机都现成 —— 换一张材质比在外面重算一遍
//! 屏幕坐标少一整摊代码,光泽也白拿卡片自身的俯仰抖动。

use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, ShaderType,
};
use bevy::shader::ShaderRef;

/// `foil.wgsl` 在资产系统里的路径。着色器随二进制内嵌(`embedded_asset!`),
/// 应用无头运行、没有 `assets/` 目录。
const SHADER_PATH: &str = "embedded://render3d/foil.wgsl";

/// 闪卡这一帧的参数。字段顺序与 `foil.wgsl` 的 `FoilParams` 手工对齐。
#[derive(Clone, Copy, Debug, ShaderType)]
pub(crate) struct FoilParams {
    /// 秒。光泽常驻流动,时间是它唯一的驱动源。
    pub time: f32,
    /// 随深度压暗的系数(1 = 原亮度),与普通卡的 base_color 同义。
    pub dim: f32,
    pub _pad: Vec2,
}

impl Default for FoilParams {
    fn default() -> Self {
        Self {
            time: 0.0,
            dim: 1.0,
            _pad: Vec2::ZERO,
        }
    }
}

/// 闪卡材质:底图是 ui 侧烘好的卡面,彩虹与高光在着色器里加。
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub(crate) struct FoilMaterial {
    #[uniform(0)]
    pub params: FoilParams,
    /// 这一格的卡面纹理(圆角、描边、投影已烘进 alpha)。
    #[texture(1)]
    #[sampler(2)]
    pub cover: Handle<Image>,
}

impl Material for FoilMaterial {
    fn fragment_shader() -> ShaderRef {
        SHADER_PATH.into()
    }

    /// 与普通卡同一档透明度,理由见 `wall::card_material`。
    fn alpha_mode(&self) -> AlphaMode {
        #[cfg(target_os = "android")]
        return AlphaMode::Mask(0.5);
        #[cfg(not(target_os = "android"))]
        return AlphaMode::Blend;
    }

    /// 卡墙是装饰层,场景里没有承影面,也没有 prepass 要喂。
    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }
}
