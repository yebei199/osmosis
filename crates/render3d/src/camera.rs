//! 相机与投影的数学:两台相机的落位、锚点到视口的换算,以及遮挡层深度。

use bevy::prelude::*;
// BSN(next-gen 场景系统,bevy_scene feature)的 bsn! 宏、Scene/SceneList、
// World::spawn_scene 都已在 bevy::prelude 里,无需额外 use。见 rebuild_content。
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。
use bevy::camera::{
    Camera3dDepthLoadOp, ClearColorConfig, RenderTarget,
};
use bevy::core_pipeline::tonemapping::Tonemapping;

use crate::scene::EMPTY_OCCLUDER_DEPTH;

/// 相机位置,全程不变。
///
/// **正对**点云平面(y=0):偏一点就把那张平面看成俯视的梯形,封面就歪了。早先那层
/// 浮空尘埃是绕卡片飘的球,俯角只是给它一点立体感;点云是一张图,正对才是对的。
///
/// 刻意**不**随视口长宽比后撤:点云是铺满视野的环境效果,后撤只会把粒子缩成看不见的
/// 点(小米13 竖屏 aspect 0.45 会后撤 2.2 倍,真机上实测粒子直接消失)。恒用这个距离,
/// 让点云自然溢出画面四边。
pub(crate) const BASE_CAMERA_POS: Vec3 =
    Vec3::new(0.0, 0.0, 8.0);

/// 摆放相机(渲染进离屏目标图),全程不变;返回相机实体供尺寸变化时改 RenderTarget。
///
/// 场景里没有光:点云的颜色直接取自封面纹理,不过光照(见 `cloud.wgsl`)。
pub(crate) fn spawn_camera(
    app: &mut App,
    target: &Handle<Image>,
) -> Entity {
    // 相机:渲染进离屏目标图,而非屏幕。0.19 起 RenderTarget 是独立组件,不再是 Camera 的字段。
    app.world_mut()
        .spawn((
            Camera3d::default(),
            Camera {
                // 粒子图要叠在 warp 背景上,没画到的像素必须透明。
                clear_color: ClearColorConfig::Custom(
                    Color::NONE,
                ),
                ..default()
            },
            RenderTarget::Image(target.clone().into()),
            // 默认的 TonyMcMapFace 需要 tonemapping_luts feature(会拉 LUT 资源)。
            // PoC 不启那个 feature,改用无需 LUT 的 None。要更好观感时再开该 feature。
            Tonemapping::None,
            // 位置全程不变,resize 也不动它(见 [`BASE_CAMERA_POS`])。
            Transform::from_translation(BASE_CAMERA_POS)
                .looking_at(Vec3::ZERO, Vec3::Y),
        ))
        .id()
}

/// 第二台相机:与主相机同位、同投影、同色调映射,渲染进**遮挡层**目标图。
///
/// 这一台是「深度正确的 UI」的全部机关。UI 侧把画面叠成三层 —— 场景、Slint 卡片、
/// 遮挡层 —— 于是卡片被场景里更近的物体逐像素挡住,而 Slint 只做了寻常的 alpha 合成:
/// 它不需要知道深度,UI 也不需要先渲进纹理。
///
/// 与主相机只差两处,合起来就是那个效果:
/// - 清除色透明:没画到的地方 alpha 为 0,合成时露出下面的卡片;
/// - 深度缓冲不清到远平面,而是清到卡片锚点的深度(每帧由 `render_frame` 填,
///   见 [`occluder_depth`]),于是只有**比卡片更近**的片元能过 `GreaterEqual`。
///
/// 逐片元是关键:一个横跨锚点平面的立方体会被平面切开,而不是整体跳到卡片前面或后面。
/// 这是 CPU 侧按物体排序做不到的,也正是「合成器把 UI 整层贴在 canvas 上」的方案
/// 在原理上做不到的那件事。
///
/// ponytail: 代价是几何被提交两遍。8~64 个形状时可忽略;真要省,可在卡片隐藏时
/// 把这台相机 `is_active` 关掉,或改成采样深度纹理的一个全屏 pass(那需要自建
/// 渲染图节点与 WGSL,现在不值)。
///
/// `order` 排在主相机之后,只为让两次渲染先后确定;二者目标不同,并无依赖。
pub(crate) fn spawn_occluder_camera(
    app: &mut App,
    target: &Handle<Image>,
) -> Entity {
    app.world_mut()
        .spawn((
            Camera3d {
                // 首帧的占位值,真值每帧由 render_frame 填。
                depth_load_op: Camera3dDepthLoadOp::Clear(
                    EMPTY_OCCLUDER_DEPTH,
                ),
                ..default()
            },
            Camera {
                order: 1,
                clear_color: ClearColorConfig::Custom(
                    Color::NONE,
                ),
                ..default()
            },
            RenderTarget::Image(target.clone().into()),
            // 必须与主相机一致:同一个物体在两层里出现时颜色要逐像素相同,
            // 否则被切开的那半会显出另一种色调,穿帮。
            Tonemapping::None,
            // 必须与主相机同位同视角,否则两层对不上,遮挡会整体错位。
            Transform::from_translation(BASE_CAMERA_POS)
                .looking_at(Vec3::ZERO, Vec3::Y),
        ))
        .id()
}

/// 遮挡层的深度清除值:锚点的 NDC 深度。
///
/// bevy 用反向 Z(1 是近平面,0 是远平面),深度测试是 `GreaterEqual`。把深度缓冲
/// 预先清到这个值,只有比锚点更近的片元能通过测试画进遮挡层。
///
/// 锚点跑到相机背后、或投影退化出非有限值时退回 [`EMPTY_OCCLUDER_DEPTH`]:遮挡层为空,
/// 卡片完整可见。宁可少一个效果,也不能把整幅场景糊在卡片上 —— 后者是刺眼的错画面。
/// wgpu 另有硬性要求:深度清除值必须落在 [0, 1],越界会被校验层拒掉。
/// 锚点在视口里的归一化位置(0..1,**左上**原点),出画或投影不出来时为 `None`。
///
/// 归一而非物理像素:离屏纹理尺寸与 UI 的逻辑像素是两套刻度,交给 UI 侧乘自己的
/// 面板尺寸,中间少一次要同步的换算。
///
/// 出画就给 `None`,不钳到边上:钳过的卡片会粘在屏幕边缘假装还指着那个物体。
pub(crate) fn anchor_viewport(
    anchor_ndc: Option<Vec3>,
) -> Option<(f32, f32)> {
    let ndc = anchor_ndc?;
    // 深度判据与遮挡层那条共用:锚点跑到相机背后或视锥之外,这一帧就没有卡片。
    if !ndc.is_finite() || !(0.0..=1.0).contains(&ndc.z) {
        return None;
    }
    // NDC 的 y 向上、UI 的 y 向下,所以 y 这一路取负。
    let x = ndc.x * 0.5 + 0.5;
    let y = 0.5 - ndc.y * 0.5;
    let on_screen = (0.0..=1.0).contains(&x)
        && (0.0..=1.0).contains(&y);
    on_screen.then_some((x, y))
}

pub(crate) fn occluder_depth(
    anchor_ndc: Option<Vec3>,
) -> f32 {
    match anchor_ndc {
        Some(ndc)
            if ndc.z.is_finite()
                && (0.0..=1.0).contains(&ndc.z) =>
        {
            ndc.z
        }
        _ => EMPTY_OCCLUDER_DEPTH,
    }
}

#[cfg(test)]
mod tests;
