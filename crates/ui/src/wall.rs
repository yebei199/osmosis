//! 卡墙的几何真相与交互状态(docs/adr/0025,handoff-readme.md 3b)。
//!
//! 与 nav_glass / aurora_btn 同一套路:**几何与动力学全在这里算**,
//! render3d 的卡墙场景只做「应用变换 + 渲染」。这样拖动惯性、格点吸附、
//! 命中测试、塌回插值全部可以无 GPU 单测,两边靠 POD seam 镜像分离。
//!
//! 坐标约定:x 右、y 下、z 朝观者为正,单位是**物理像素**;透视遵循
//! CSS `perspective: 1200px` 的模型(相机距 z=0 平面 1200 单位,按
//! 容器宽等比缩放)。render3d 侧负责换算成 bevy 的 y 上坐标系。

/// 卡墙最多接管的卡数。12 张占一屏,三屏够拖着翻;再多的曲目走列表。
/// ponytail: 硬上限,曲目分页进卡墙等真需要再做。
pub const WALL_MAX_CARDS: usize = 36;

/// 参考场里透视距离与场宽之比(1200 / 904,handoff 3b)。
const PERSPECTIVE_RATIO: f32 = 1200.0 / 904.0;

/// 一步塌回/相机收敛里,认为「到位」的阈值(像素或弧度都用它,量级合适)。
const SETTLED: f32 = 0.01;

/// 布局参数,全部由容器尺寸推出(设计稿硬规则:坐标不跨尺寸复用)。
/// 参考场 904×432:卡 150、列距 204、行距 136、奇数行右移 26、z ∈ [-240, +80]。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WallLayout {
    pub card: f32,
    pub col_pitch: f32,
    pub row_pitch: f32,
    pub odd_shift: f32,
    pub rows: usize,
    pub z_min: f32,
    pub z_max: f32,
    /// 透视距离(CSS perspective),随容器宽缩放。
    pub perspective: f32,
    /// 场区尺寸,物理像素。
    pub w: f32,
    pub h: f32,
}

mod anim;
mod camera;

pub use anim::{Collapse, DollyRun};
pub use camera::{
    WallCam, hit_test, project, world_pose,
};

/// 按容器尺寸算布局。紧凑版式卡更小、行距更密(一屏约 2 列)。
pub fn layout(w: f32, h: f32, compact: bool) -> WallLayout {
    // 参考场 904×432 的各项比例。行数固定 3,列数由卡数决定(横向翻找)。
    let unit = if compact { w / 420.0 } else { w / 904.0 };
    WallLayout {
        card: 150.0
            * unit
            * if compact { 0.48 } else { 1.0 },
        col_pitch: 204.0
            * unit
            * if compact { 0.52 } else { 1.0 },
        // 行距与列距同源(按宽),不按高:场区是整页高,按高缩放会把
        // 三行拉到天各一方 —— 布局形状恒定,竖向多余空间留白。
        row_pitch: 136.0
            * unit
            * if compact { 0.52 } else { 1.0 },
        odd_shift: 26.0 * unit,
        rows: 3,
        z_min: -240.0 * unit,
        z_max: 80.0 * unit,
        perspective: w * PERSPECTIVE_RATIO,
        w,
        h,
    }
}

/// 一张卡的世界位姿。`dim` 是随深度压暗的系数(1 = 原亮度)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardPose {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// 弧度。
    pub rot_y: f32,
    pub rot_x: f32,
    pub dim: f32,
}

/// 确定性伪随机:同一张卡每帧、每次启动散布位置一致,卡墙才不会闪。
fn jitter(index: usize, salt: u32) -> f32 {
    let h = (index as u32)
        .wrapping_add(salt.wrapping_mul(0x9e37_79b9))
        .wrapping_mul(0x85eb_ca6b);
    let h = (h ^ (h >> 13)).wrapping_mul(0xc2b2_ae35);
    ((h >> 8) & 0xffff) as f32 / 65535.0
}

/// 第 `index` 张卡在塌回系数 `collapse` 下的位姿。
///
/// `collapse` = 1 是卡墙(z 散开、带旋转抖动),0 是列表(全部塌回 z=0
/// 排成平面网格)。中间值线性插值 —— 「列表与卡墙是同一批卡」的具体形式。
/// 位置以场中心为原点。
pub fn card_pose(
    lay: &WallLayout,
    index: usize,
    collapse: f32,
) -> CardPose {
    let row = index % lay.rows;
    let col = index / lay.rows;
    let odd =
        if row % 2 == 1 { lay.odd_shift } else { 0.0 };
    // 网格基准位:三行居中,列从场左往右铺。
    let x = col as f32 * lay.col_pitch + odd - lay.w * 0.28;
    let y = (row as f32 - (lay.rows as f32 - 1.0) / 2.0)
        * lay.row_pitch;

    // 卡墙态的深度散布与旋转抖动(handoff 3b 的范围)。
    let z = lay.z_min
        + jitter(index, 1) * (lay.z_max - lay.z_min);
    let rot_y =
        (-16.0 + jitter(index, 2) * 30.0).to_radians();
    let rot_x =
        (-5.0 + jitter(index, 3) * 12.0).to_radians();

    // 越远越暗:z_min 处 0.55,z_max 处 1.0。
    let depth01 = (z - lay.z_min) / (lay.z_max - lay.z_min);
    let dim_wall = 0.55 + 0.45 * depth01;

    let t = collapse.clamp(0.0, 1.0);
    CardPose {
        x,
        y,
        z: z * t,
        rot_y: rot_y * t,
        rot_x: rot_x * t,
        dim: 1.0 + (dim_wall - 1.0) * t,
    }
}

/// 把一张 RGBA 缩略图的四角按圆角半径抠成透明(在 CPU 上烘,
/// 免得 render3d 为圆角开一条自定义材质)。半径按卡片比例 14/150。
pub fn bake_rounded(rgba: &mut [u8], w: u32, h: u32) {
    let r = (w.min(h) as f32) * (14.0 / 150.0);
    let (fw, fh) = (w as f32, h as f32);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;
            // 到最近角圆心的距离;不在角区的像素距离为 0。
            let dx = (r - fx).max(fx - (fw - r)).max(0.0);
            let dy = (r - fy).max(fy - (fh - r)).max(0.0);
            let d = (dx * dx + dy * dy).sqrt();
            if d > r - 0.5 {
                let a = (r + 0.5 - d).clamp(0.0, 1.0);
                let i = ((y * w + x) * 4 + 3) as usize;
                rgba[i] = (f32::from(rgba[i]) * a) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests;
