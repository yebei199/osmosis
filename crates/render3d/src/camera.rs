//! 相机:播放页那一台的落位,以及按版式给它装的「移轴镜头」。

use bevy::prelude::*;
// BSN(next-gen 场景系统,bevy_scene feature)的 bsn! 宏、Scene/SceneList、
// World::spawn_scene 都已在 bevy::prelude 里,无需额外 use。见 rebuild_content。
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。
use bevy::camera::{
    ClearColorConfig, RenderTarget, SubCameraView,
};
use bevy::core_pipeline::tonemapping::Tonemapping;

/// 相机位置,全程不变。
///
/// **正对**点云平面(y=0):偏一点就把那张平面看成俯视的梯形,封面就歪了。封面实物
/// 要的那一点俯视不靠挪相机,靠把它自己那座台面往前倾(见 `sleeve::motion::STAGE_PITCH`)——
/// 两个效果共用这一台相机,谁也不该为对方的取景改它。
///
/// 刻意**不**随视口长宽比后撤:点云是铺满视野的环境效果,后撤只会把粒子缩成看不见的
/// 点(小米13 竖屏 aspect 0.45 会后撤 2.2 倍,真机上实测粒子直接消失)。恒用这个距离,
/// 让点云自然溢出画面四边;封面实物自己按视口收尺寸(`sleeve::motion::stage_scale`)。
pub(crate) const BASE_CAMERA_POS: Vec3 =
    Vec3::new(0.0, 0.0, 8.0);

/// 竖直视野,即 bevy 透视投影的默认值。移轴时按它反推放大后的视野(见 [`lens`])。
pub(crate) const BASE_FOV: f32 =
    std::f32::consts::FRAC_PI_4;

/// 宽版式下主体往左让多少,以窗口宽为单位。
///
/// 右边 41.5% 起是歌名、歌手与歌词那一栏(`playpage.slint` 的 `text-x`),主体中心
/// 落在 0.3 处,两边不打架。这个数原先是 `.slint` 里把整张场景图往左推的 `-0.2`,
/// 推出去的右边那 20% 于是没有任何 3D 内容,出界的东西在那条边上被硬切 —— 现在由
/// 镜头平移做,图本身铺满整窗。
pub(crate) const WIDE_SHIFT: f32 = 0.2;

/// 紧凑版式下主体往上让多少,以窗口高为单位。字摆在主体下面(同上,原是 `-0.14`)。
pub(crate) const COMPACT_SHIFT: f32 = 0.14;

/// 摆放相机(渲染进离屏目标图);返回相机实体供尺寸变化时改 RenderTarget、换版式时改镜头。
///
/// 场景里没有 bevy 的光:点云的颜色直接取自封面纹理,封面实物的那盏主光写死在
/// 它自己的着色器里(见 `sleeve.wgsl`)。
pub(crate) fn spawn_camera(
    app: &mut App,
    target: &Handle<Image>,
) -> Entity {
    // 相机:渲染进离屏目标图,而非屏幕。0.19 起 RenderTarget 是独立组件,不再是 Camera 的字段。
    app.world_mut()
        .spawn((
            Camera3d::default(),
            Camera {
                // 场景要叠在播放页的糊封面背景上,没画到的像素必须透明。
                clear_color: ClearColorConfig::Custom(
                    Color::NONE,
                ),
                ..default()
            },
            // 显式给出透视投影:移轴要改它的视野(见 [`lens`]),得有一份确定的起点。
            Projection::Perspective(
                PerspectiveProjection {
                    fov: BASE_FOV,
                    ..default()
                },
            ),
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

/// 主轴离窗口中心偏多少 (x 往左, y 往上),以窗宽 / 窗高为单位:宽版式横移、紧凑版式纵移。
///
/// 镜头([`lens`])与点云的场(`cloud::Framing`)都按它算视口框住了哪一块,
/// 两边不各写一份。
pub(crate) fn shifts(compact: bool) -> Vec2 {
    if compact {
        Vec2::new(0.0, COMPACT_SHIFT)
    } else {
        Vec2::new(WIDE_SHIFT, 0.0)
    }
}

/// 一台相机的「镜头」:子视图(移轴)与竖直视野。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Lens {
    /// `None` 表示不移轴,直接用整幅视野。
    pub sub_view: Option<SubCameraView>,
    /// 竖直视野,弧度。
    pub fov: f32,
}

/// 按版式给相机配镜头:主体在宽版式偏左、紧凑版式偏上,**图本身铺满整窗**。
///
/// 这是移轴(off-axis)投影:假想一幅更大的画面,主轴仍在它正中,渲染的只是其中偏到
/// 一边的那一窗。效果与「把居中渲好的图整张平移」逐像素相同 —— 点云因此一点不变 ——
/// 区别在于平移出来的那一边也有内容,不再是一条空带。物体也不会因为离开画面中心
/// 而被斜着看:视线方向始终是 -z,封面的正面仍投成正的矩形。
///
/// bevy 的 [`SubCameraView`] 把竖直视野对齐到**整幅**画面的高。横向移轴不改高,
/// 视野照旧;纵向移轴把整幅加高了 `1 + 2·shift` 倍,视野得按同一倍数放大,否则
/// 窗里的东西会跟着放大。
///
/// 0 尺寸(首帧、刚重建)给不移轴的默认镜头 —— 那一帧本来也画不出东西。
pub(crate) fn lens(
    width: u32,
    height: u32,
    compact: bool,
) -> Lens {
    let Vec2 {
        x: shift_x,
        y: shift_y,
    } = shifts(compact);
    if width == 0 || height == 0 {
        return Lens {
            sub_view: None,
            fov: BASE_FOV,
        };
    }
    let (w, h) = (width as f32, height as f32);
    let grow_x = 1.0 + 2.0 * shift_x;
    let grow_y = 1.0 + 2.0 * shift_y;
    Lens {
        sub_view: Some(SubCameraView {
            full_size: UVec2::new(
                (w * grow_x).round() as u32,
                (h * grow_y).round() as u32,
            ),
            // 窗口贴着整幅的右(下)沿:主轴于是落在窗里 0.5 − shift 处。
            offset: Vec2::new(
                2.0 * shift_x * w,
                2.0 * shift_y * h,
            ),
            size: UVec2::new(width, height),
        }),
        fov: 2.0 * (grow_y * (BASE_FOV * 0.5).tan()).atan(),
    }
}

#[cfg(test)]
mod tests;
