//! 卡墙的几何真相与交互状态(docs/adr/0025,handoff-readme.md 3b)。
//!
//! 与 nav_glass / aurora_btn 同一套路:**几何与动力学全在这里算**,
//! render3d 的卡墙场景只做「应用变换 + 渲染」。这样拖动惯性、环面回绕、
//! 命中测试、塌回插值全部可以无 GPU 单测,两边靠 POD seam 镜像分离。
//!
//! 坐标约定:x 右、y 下、z 朝观者为正,单位是**物理像素**;透视遵循
//! CSS `perspective: 1200px` 的模型(相机距 z=0 平面 1200 单位,按
//! 容器宽等比缩放)。render3d 侧负责换算成 bevy 的 y 上坐标系。
//!
//! 网格是**环面**:两轴都按周期取模,往任何方向拖到底都会回到同一批卡。
//! 每张卡仍然只有一个实体 —— 回绕发生在「卡相对相机的位置」上,滑出右边
//! 的那张就是从左边进来的那张。

/// 卡墙最多接管的卡数。再多的曲目走列表。
/// ponytail: 硬上限,曲目分页进卡墙等真需要再做。
pub const WALL_MAX_CARDS: usize = 36;

/// 卡片纹理四周留出的边距,占卡边长的比例。投影画在这一圈里 ——
/// 方片本身没有卡外像素可用,阴影只能靠把纹理撑大一圈来装。
pub const CARD_PAD: f32 = 0.16;

/// 参考场里透视距离与场宽之比(1200 / 904,handoff 3b)。
const PERSPECTIVE_RATIO: f32 = 1200.0 / 904.0;

/// 一步塌回/相机收敛里,认为「到位」的阈值(像素或弧度都用它,量级合适)。
const SETTLED: f32 = 0.01;

/// 布局参数,全部由容器尺寸与卡数推出(设计稿硬规则:坐标不跨尺寸复用)。
/// 参考场 904×432:卡 150、列距 204、行距 136、奇数行右移 26、z ∈ [-240, +80]。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WallLayout {
    pub card: f32,
    pub col_pitch: f32,
    pub row_pitch: f32,
    pub odd_shift: f32,
    pub rows: usize,
    pub cols: usize,
    pub z_min: f32,
    pub z_max: f32,
    /// 透视距离(CSS perspective),随容器宽缩放。
    pub perspective: f32,
    /// 场区尺寸,物理像素。
    pub w: f32,
    pub h: f32,
}

impl WallLayout {
    /// 环面在 x 上的周期。
    pub fn span_x(&self) -> f32 {
        self.cols as f32 * self.col_pitch
    }

    /// 环面在 y 上的周期。
    pub fn span_y(&self) -> f32 {
        self.rows as f32 * self.row_pitch
    }
}

mod anim;
mod camera;

pub use anim::{Collapse, DollyRun};
pub use camera::{WallCam, hit_test, project, world_pose};

/// 按容器尺寸与卡数算布局。紧凑版式卡更小、行距更密。
///
/// 行列数按**场区长宽比**分配而不是死取方形:一屏三行是横屏卡墙的形状,
/// 竖屏手机上照搬就成了一条横带。
///
/// 周期按「罩住**最远那层卡**上的视口 + 两张卡」撑开。
///
/// 不按 z=0 那层:越远的卡投影越小,同一块屏幕在它那一层对应的世界范围就越大
/// —— 按近处算出来的周期,回绕会发生在屏幕**里**,远处的卡凭空冒出来。
///
/// 两张卡而不是一张:一张卡只够让边界落在「刚好完全出屏」那一点上,而卡是
/// 一帧一帧挪的,越过边界前的最后一帧它还差一点没出去。多留一张卡的余量,
/// 回绕前它已经在屏外走了一段。这正是环面唯一的前提:回绕在屏幕上什么也没发生。
pub fn layout(
    w: f32,
    h: f32,
    compact: bool,
    count: usize,
) -> WallLayout {
    let unit = if compact { w / 420.0 } else { w / 904.0 };
    let shrink = if compact { 0.52 } else { 1.0 };
    let card =
        150.0 * unit * if compact { 0.48 } else { 1.0 };
    let base_col = 204.0 * unit * shrink;
    // 行距与列距同源(按宽),不按高:布局形状恒定,竖向靠周期撑开而不是拉行距。
    let base_row = 136.0 * unit * shrink;

    let n = count.clamp(1, WALL_MAX_CARDS);
    let (fw, fh) = (w.max(1.0), h.max(1.0));
    // 期望的 列/行 之比:让网格的像素外形贴近场区外形。
    let ratio = (fw * base_row) / (fh * base_col);
    let cols = ((n as f32 * ratio).sqrt().round() as usize)
        .clamp(1, n);
    let rows = n.div_ceil(cols);

    // 视口投到最远那层卡上要放大这么多倍。
    let z_min = -240.0 * unit;
    let perspective = w * PERSPECTIVE_RATIO;
    let far = 1.0 + (-z_min) / perspective.max(1.0);

    WallLayout {
        card,
        col_pitch: base_col
            .max((fw * far + card * 2.0) / cols as f32),
        row_pitch: base_row
            .max((fh * far + card * 2.0) / rows as f32),
        odd_shift: 26.0 * unit,
        rows,
        cols,
        z_min,
        z_max: 80.0 * unit,
        perspective,
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

/// 第 `index` 张卡在塌回系数 `collapse` 下的**网格位姿**。
///
/// `collapse` = 1 是卡墙(z 散开、带旋转抖动),0 是列表(全部塌回 z=0
/// 排成平面网格)。中间值线性插值 —— 「列表与卡墙是同一批卡」的具体形式。
///
/// 平面坐标是网格里的绝对位置,不减相机平移:回绕与居中都在
/// [`world_pose`] 里一次做完,那里才知道相机在哪。
pub fn card_pose(
    lay: &WallLayout,
    index: usize,
    collapse: f32,
) -> CardPose {
    let row = index % lay.rows;
    let col = index / lay.rows;
    let odd =
        if row % 2 == 1 { lay.odd_shift } else { 0.0 };
    let x = col as f32 * lay.col_pitch + odd;
    let y = row as f32 * lay.row_pitch;

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

/// 到圆角矩形边界的有符号距离(内负外正)。中心 `(cx, cy)`、半边
/// `(hx, hy)`、圆角 `r`,单位像素。
fn round_rect_sd(
    px: f32,
    py: f32,
    cx: f32,
    cy: f32,
    hx: f32,
    hy: f32,
    r: f32,
) -> f32 {
    let dx = (px - cx).abs() - (hx - r);
    let dy = (py - cy).abs() - (hy - r);
    let (ax, ay) = (dx.max(0.0), dy.max(0.0));
    (ax * ax + ay * ay).sqrt() + dx.max(dy).min(0.0) - r
}

/// 把一张 RGBA 缩略图烘成**卡面**:圆角 + 细描边 + 四周一圈柔和投影。
///
/// 返回 (像素, 宽, 高)。输出比输入四周各大 [`CARD_PAD`] —— 方片没有卡外
/// 像素,投影只能靠撑大纹理来装,render3d 侧的方片也按同样比例放大。
/// 全在 CPU 上烘:每张封面只烘一次,免得为一圈描边给每张卡开自定义材质。
pub fn bake_card(
    rgba: &[u8],
    w: u32,
    h: u32,
) -> (Vec<u8>, u32, u32) {
    let pad = ((w.min(h) as f32 * CARD_PAD).round() as u32)
        .max(2);
    let (ow, oh) = (w + pad * 2, h + pad * 2);
    let (fw, fh) = (w as f32, h as f32);
    let fpad = pad as f32;
    // 圆角半径沿用设计稿的 14/150。
    let radius = fw.min(fh) * (14.0 / 150.0);
    // 描边宽度与投影参数都按卡边长走,不同尺寸的缩略图观感才一致。
    let stroke = (fw.min(fh) / 110.0).max(1.0);
    let blur = fpad * 0.9;
    let drop = fpad * 0.34;

    let (cx, cy) = (ow as f32 * 0.5, oh as f32 * 0.5);
    let (hx, hy) = (fw * 0.5, fh * 0.5);
    let mut out = vec![0u8; (ow * oh * 4) as usize];
    for y in 0..oh {
        for x in 0..ow {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let sd = round_rect_sd(
                px, py, cx, cy, hx, hy, radius,
            );
            // 卡面覆盖率:1px 抗锯齿过渡。
            let cov = (0.5 - sd).clamp(0.0, 1.0);
            // 投影:同一个圆角矩形往下挪一点再糊开。
            let sh = round_rect_sd(
                px,
                py - drop,
                cx,
                cy,
                hx,
                hy,
                radius,
            );
            let t = (1.0 - sh / blur).clamp(0.0, 1.0);
            let shadow = 0.42 * t * t * (3.0 - 2.0 * t);

            let o = ((y * ow + x) * 4) as usize;
            let mut rgb = [0.0f32; 3];
            if cov > 0.0 {
                let sx = (x.saturating_sub(pad)).min(w - 1);
                let sy = (y.saturating_sub(pad)).min(h - 1);
                let i = ((sy * w + sx) * 4) as usize;
                for c in 0..3 {
                    rgb[c] = f32::from(rgba[i + c]);
                }
                // 细描边:贴着内边缘提亮,像卡片受光的那一圈。
                let edge = ((sd + stroke) / stroke)
                    .clamp(0.0, 1.0);
                for c in rgb.iter_mut() {
                    *c += (255.0 - *c) * 0.55 * edge;
                }
            }
            // 卡面压在投影上(投影纯黑,直通道合成)。
            let a = cov + shadow * (1.0 - cov);
            if a > 0.0 {
                for c in 0..3 {
                    out[o + c] = (rgb[c] * cov / a)
                        .clamp(0.0, 255.0)
                        as u8;
                }
            }
            out[o + 3] =
                (a * 255.0).clamp(0.0, 255.0) as u8;
        }
    }
    (out, ow, oh)
}

/// 封面还没到时那张卡的底纹:纯白卡面走同一条烘焙,颜色交给 render3d
/// 的占位底色去乘。有它才不会在封面到货前露出一个没圆角、没投影的方块。
pub fn bake_blank(size: u32) -> (Vec<u8>, u32, u32) {
    let white = vec![255u8; (size * size * 4) as usize];
    bake_card(&white, size, size)
}

#[cfg(test)]
mod tests;
