//! 被标注的那个物体:一枚绕轨道慢转的方块,以及它的位姿计算。
//!
//! 它存在的理由是点云当不了被标注物 —— 三万颗无特征粒子没有「一个东西」可指,
//! 而把卡片锚在封面平面上会被半数粒子埋掉(见 `lib.rs` 的 `CARD_ANCHOR_LOCAL`
//! 那段历史)。方块给了三样点云给不了的东西:确定的深度、可指的边界,以及
//! 一条自己会走的轨迹 —— 遮挡因此不必等人来拖点云才演得出来。
//!
//! 轨道近端离开封面平面(卡片读得成),远端贴回平面(粒子成片从它前面过)。
//! 两端都由 `lib.rs` 的单测按 bevy 自己的投影矩阵钉住。

use bevy::prelude::*;

/// 方块的半边长(世界单位)。
///
/// 封面平面深度上 1 个世界单位约合 170 逻辑像素,所以 0.18 的半边长在竖屏上
/// 约 61 像素见方 —— 看得见是个实体,又不至于压住封面。
pub(crate) const MARKER_HALF: f32 = 0.18;

/// 轨道中心,世界坐标。y 抬高一点,免得它正压在封面中心的主体上。
///
/// z 取封面平面(`CLOUD_ORIGIN.z`)之前 0.9:配上 [`ORBIT_RADIUS`],
/// 轨道的 z 就落在 2.15~3.25,近端离平面 1.45(高过粒子位移峰值 1.2~1.5,
/// 卡片干净),远端只离 0.35(远小于峰值,粒子成片穿过)。
pub(crate) const ORBIT_CENTER: Vec3 =
    Vec3::new(0.0, 0.5, 2.7);

/// 轨道半径。
///
/// 上限不是「锚点留在画面里」而是「**卡片整块**留在画面里」—— 锚点是卡片的中心,
/// 它贴着边时卡片已经探出去半个身位了(0.7 那一版真机实拍,右边缘切掉约 20%)。
/// 竖屏近端 1 世界单位约合 222 逻辑像素,窗口半宽 196,卡片半宽 66,于是
/// 半径不得超过 (196 − 66) / 222 ≈ 0.58。取 0.55 留一点余量。
pub(crate) const ORBIT_RADIUS: f32 = 0.55;

/// 转一圈用多少秒。慢到不抢戏,又快到不必盯着看才发现它在动。
pub(crate) const ORBIT_PERIOD: f32 = 20.0;

/// 标记体这一帧的位姿。`time` 是播放页时钟(秒,门关即冻结)。
///
/// 只平移不自转:自转会让「前表面」的位置随姿态变,而卡片的锚点正钉在那儿
/// (见 [`front_face`])。一个不转的轴对齐方块,前表面永远是 `+z` 那一面。
pub(crate) fn pose(time: f32) -> Transform {
    let angle = std::f32::consts::TAU * time / ORBIT_PERIOD;
    Transform::from_translation(
        ORBIT_CENTER
            + Vec3::new(
                ORBIT_RADIUS * angle.sin(),
                0.0,
                ORBIT_RADIUS * angle.cos(),
            ),
    )
}

/// 锚点要比前表面再往相机挪这么一点。
///
/// 遮挡层的深度测试是 `GreaterEqual`,**含等号**:锚点正落在前表面上时,前表面
/// 自己的片元深度恰好等于门槛,于是通过测试、被画进遮挡层 —— 方块盖住自己的
/// 标签(2026-08-13 真机实拍)。挪开一点,等号就不成立了。
///
/// 0.02 世界单位:在这个距离上约合 4 个逻辑像素的视差(看不出来),换算到反向 Z
/// 的 NDC 上是 9e-5,而 Depth32Float 在门槛那一档(约 0.02)的精度是 1e-7 量级,
/// 富余三个数量级。
const FRONT_MARGIN: f32 = 0.02;

/// 卡片挂的那一点:方块**前表面**之前一点。
///
/// 不取几何中心 —— 那样方块自己的前半比锚点更近,会被画进遮挡层,于是它盖住
/// 自己的标签。相机在 +z 一侧(见 `BASE_CAMERA_POS`),所以前表面是 `+z` 那面。
/// 也不取前表面本身,理由见 [`FRONT_MARGIN`]。
pub(crate) fn front_face(pose: &Transform) -> Vec3 {
    pose.translation + Vec3::Z * (MARKER_HALF + FRONT_MARGIN)
}

/// 方块的网格与材质。
///
/// 材质走 `unlit`:场景里没有光(点云的颜色直接取自封面纹理,不过光照),
/// 为这一个方块引一盏灯要多一份要调的东西,而这里要的只是一个看得见的实体。
pub(crate) fn spawn(app: &mut App) -> Entity {
    let mesh = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::from_length(MARKER_HALF * 2.0));
    let material = app
        .world_mut()
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial {
            base_color: Color::srgb(0.86, 0.92, 0.84),
            unlit: true,
            ..default()
        });
    app.world_mut()
        .spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            pose(0.0),
        ))
        .id()
}
