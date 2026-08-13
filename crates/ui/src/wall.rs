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

/// 按容器尺寸算布局。紧凑版式卡更小、行距更密(一屏约 2 列)。
pub fn layout(w: f32, h: f32, compact: bool) -> WallLayout {
    // 参考场 904×432 的各项比例。行数固定 3,列数由卡数决定(横向翻找)。
    let unit = if compact { w / 420.0 } else { w / 904.0 };
    WallLayout {
        card: 150.0 * unit * if compact { 0.48 } else { 1.0 },
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
    let odd = if row % 2 == 1 { lay.odd_shift } else { 0.0 };
    // 网格基准位:三行居中,列从场左往右铺。
    let x = col as f32 * lay.col_pitch + odd
        - lay.w * 0.28;
    let y = (row as f32 - (lay.rows as f32 - 1.0) / 2.0)
        * lay.row_pitch;

    // 卡墙态的深度散布与旋转抖动(handoff 3b 的范围)。
    let z = lay.z_min
        + jitter(index, 1) * (lay.z_max - lay.z_min);
    let rot_y = (-16.0 + jitter(index, 2) * 30.0)
        .to_radians();
    let rot_x =
        (-5.0 + jitter(index, 3) * 12.0).to_radians();

    // 越远越暗:z_min 处 0.55,z_max 处 1.0。
    let depth01 =
        (z - lay.z_min) / (lay.z_max - lay.z_min);
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

/// 卡墙相机:拖动平移(带惯性 + 格点吸附)、拖动微转 yaw、滚轮 dolly。
///
/// 全部按帧收敛(与 `aurora_btn::ButtonAnim` 同风格,不带 dt):
/// 60fps 下惯性阻尼 0.92 与回弹系数的手感即设计稿标定值。
#[derive(Clone, Copy, Debug)]
pub struct WallCam {
    pub pan_x: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub dolly: f32,
    vel_x: f32,
    dragging: bool,
}

impl Default for WallCam {
    fn default() -> Self {
        Self {
            pan_x: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            dolly: 0.0,
            vel_x: 0.0,
            dragging: false,
        }
    }
}

/// 拖动惯性的阻尼(handoff 交互表)。
const DRAG_DAMPING: f32 = 0.92;
/// 松手后向吸附点靠拢的每帧比例。
const SNAP_CONVERGE: f32 = 0.14;
/// yaw 随拖动速度的比例与上限(±14°,别把墙拖成侧面)。
const YAW_PER_VEL: f32 = 0.0016;
const YAW_MAX: f32 = 0.244;
/// dolly 行程限制:太近穿卡,太远全糊。
const DOLLY_MIN: f32 = -260.0;
const DOLLY_MAX: f32 = 620.0;

impl WallCam {
    /// 指针拖动一帧的位移(物理像素)。
    pub fn drag(&mut self, dx: f32, dy: f32) {
        self.dragging = true;
        self.pan_x -= dx;
        self.vel_x = -dx;
        self.pitch = (self.pitch - dy * 0.0009)
            .clamp(-0.12, 0.12);
    }

    pub fn release(&mut self) {
        self.dragging = false;
    }

    /// 滚轮:沿 z 推拉。
    pub fn wheel(&mut self, delta: f32) {
        self.dolly = (self.dolly + delta)
            .clamp(DOLLY_MIN, DOLLY_MAX);
    }

    /// 松手后 pan 吸附到的格点(列距的整数倍)。
    pub fn snap_target(&self, col_pitch: f32) -> f32 {
        (self.pan_x / col_pitch).round() * col_pitch
    }

    /// 走一帧,返回**还在动吗**(动 = 这一帧要重渲)。
    pub fn step(&mut self, col_pitch: f32) -> bool {
        if self.dragging {
            // 拖动中 yaw 跟着速度偏,pitch 已在 drag 里更新。
            self.yaw = (self.vel_x * YAW_PER_VEL * 60.0)
                .clamp(-YAW_MAX, YAW_MAX);
            return true;
        }
        // 惯性滑行 + 吸附 + 姿态回正。
        self.vel_x *= DRAG_DAMPING;
        self.pan_x += self.vel_x;
        let snap = self.snap_target(col_pitch);
        self.pan_x +=
            (snap - self.pan_x) * SNAP_CONVERGE;
        self.yaw *= 0.85;
        self.pitch *= 0.90;

        let moving = self.vel_x.abs() > SETTLED
            || (snap - self.pan_x).abs() > SETTLED * 10.0
            || self.yaw.abs() > SETTLED
            || self.pitch.abs() > SETTLED;
        if !moving {
            self.pan_x = snap;
            self.yaw = 0.0;
            self.pitch = 0.0;
            self.vel_x = 0.0;
        }
        moving
    }
}

/// 整场基础俯仰(CSS `rotateX(6deg)`):墙顶微微向后倒。
pub const BASE_PITCH: f32 = 6.0 * core::f32::consts::PI / 180.0;

/// 把网格位姿变换到世界:绕场心先 yaw 后 pitch(基础 6° + 用户拖出的),
/// 卡自身的抖动旋转与整场旋转小角度合成(直接相加,量级都在 ±20° 内)。
///
/// **这是 ui 与 render3d 之间唯一的变换真相**:render3d 只把结果摆进场景,
/// 相机不带任何旋转,命中测试与渲染因此天然一致。
pub fn world_pose(
    cam: &WallCam,
    pose: &CardPose,
) -> CardPose {
    let (sin_y, cos_y) = cam.yaw.sin_cos();
    let x1 = pose.x * cos_y + pose.z * sin_y;
    let z1 = -pose.x * sin_y + pose.z * cos_y;

    let pitch = BASE_PITCH + cam.pitch;
    let (sin_p, cos_p) = pitch.sin_cos();
    // y 向下为正,顶边(y 负)向后(z 负)倒。
    let y2 = pose.y * cos_p + z1 * sin_p;
    let z2 = -pose.y * sin_p + z1 * cos_p;

    CardPose {
        x: x1,
        y: y2,
        z: z2,
        rot_y: pose.rot_y + cam.yaw,
        rot_x: pose.rot_x + pitch,
        dim: pose.dim,
    }
}

/// 把一张**世界位姿**的卡投到屏幕:返回(中心 x、中心 y、半边长),
/// 贴到近平面上给 None。相机在 (pan_x, 0.08h) 处、距 z=0 平面
/// `perspective - dolly`,视轴不带旋转 —— 透视原点 42% 的效果由相机
/// 下移 8% 实现,render3d 的相机严格同构。
pub fn project(
    lay: &WallLayout,
    cam: &WallCam,
    world: &CardPose,
) -> Option<(f32, f32, f32)> {
    let d = lay.perspective;
    let dist = (d - cam.dolly) - world.z;
    if dist <= 1.0 {
        return None;
    }
    let s = d / dist;
    let cam_y = 0.08 * lay.h;
    Some((
        lay.w * 0.5 + (world.x - cam.pan_x) * s,
        lay.h * 0.5 + (world.y - cam_y) * s,
        lay.card * 0.5 * s,
    ))
}

/// 命中测试:屏幕点 `(px, py)` 落在哪张卡上。输入**网格位姿**,内部
/// 变换到世界再投影;深度更近(z 大)的优先。
pub fn hit_test(
    lay: &WallLayout,
    cam: &WallCam,
    poses: &[CardPose],
    px: f32,
    py: f32,
) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, pose) in poses.iter().enumerate() {
        let world = world_pose(cam, pose);
        let Some((sx, sy, half)) =
            project(lay, cam, &world)
        else {
            continue;
        };
        if (px - sx).abs() <= half
            && (py - sy).abs() <= half
            && best.is_none_or(|(_, bz)| world.z > bz)
        {
            best = Some((i, world.z));
        }
    }
    best.map(|(i, _)| i)
}

/// 双击进播放页的相机 dolly:420ms ease-out,到位报「落位」,
/// Slint 层拿到落位信号才开播放页(设计稿:落位后才起点云)。
#[derive(Clone, Copy, Debug)]
pub struct DollyRun {
    /// 0 → 1。
    pub t: f32,
    /// 目标卡的深度,推向它。
    pub target_z: f32,
}

impl DollyRun {
    /// 每帧步进,返回 true = 已落位。
    pub fn step(&mut self) -> bool {
        // 420ms @60fps ≈ 25 帧;指数 ease-out,尾端并入落位判定。
        self.t += (1.0 - self.t) * 0.16;
        if self.t > 0.985 {
            self.t = 1.0;
        }
        self.t >= 1.0
    }

    /// 当前应叠加到相机上的 dolly 量。
    pub fn dolly(&self, lay: &WallLayout) -> f32 {
        // 推到卡前一点(留 0.35 倍透视距离),不穿卡。
        let goal = lay.perspective * 0.35 + self.target_z;
        goal * self.t
    }
}

/// 塌回插值:列表 0 ⇄ 卡墙 1,420ms ease-out(按帧指数收敛)。
#[derive(Clone, Copy, Debug)]
pub struct Collapse {
    pub value: f32,
    pub target: f32,
}

impl Default for Collapse {
    fn default() -> Self {
        // 每次开局回卡墙(adr/0025:视图选择不持久化)。
        Self { value: 1.0, target: 1.0 }
    }
}

impl Collapse {
    /// 走一帧,返回还在动吗。
    pub fn step(&mut self) -> bool {
        let diff = self.target - self.value;
        if diff.abs() <= SETTLED {
            self.value = self.target;
            return false;
        }
        self.value += diff * 0.16;
        true
    }
}

/// 把一张 RGBA 缩略图的四角按圆角半径抠成透明(在 CPU 上烘,
/// 免得 render3d 为圆角开一条自定义材质)。半径按卡片比例 14/150。
pub fn bake_rounded(
    rgba: &mut [u8],
    w: u32,
    h: u32,
) {
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
                rgba[i] =
                    (f32::from(rgba[i]) * a) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide() -> WallLayout {
        layout(1808.0, 864.0, false)
    }

    /// 布局全部按容器尺寸推导:同比放大容器,布局同比放大。
    #[test]
    fn layout_scales_with_container() {
        let a = layout(904.0, 432.0, false);
        let b = layout(1808.0, 864.0, false);
        assert!((b.card - a.card * 2.0).abs() < 0.01);
        assert!(
            (b.col_pitch - a.col_pitch * 2.0).abs() < 0.01
        );
        assert!(
            (b.row_pitch - a.row_pitch * 2.0).abs() < 0.01
        );
        // 参考尺寸下取回设计稿原值。
        assert!((a.card - 150.0).abs() < 0.01);
        assert!((a.col_pitch - 204.0).abs() < 0.01);
    }

    /// 紧凑版式卡更小、列距更密:420 宽下一屏能放下约两列。
    #[test]
    fn compact_layout_fits_two_columns() {
        let lay = layout(420.0, 700.0, true);
        assert!(lay.col_pitch * 2.0 < 420.0);
        assert!(lay.card < 90.0);
    }

    /// 散布确定性:同一张卡两次求位姿逐位相等,卡墙不许闪。
    #[test]
    fn poses_are_deterministic() {
        let lay = wide();
        assert_eq!(
            card_pose(&lay, 7, 1.0),
            card_pose(&lay, 7, 1.0)
        );
    }

    /// 卡墙态的散布落在设计稿范围内;塌回态深度与旋转全部归零。
    #[test]
    fn collapse_flattens_every_card() {
        let lay = wide();
        for i in 0..WALL_MAX_CARDS {
            let wall = card_pose(&lay, i, 1.0);
            assert!(
                wall.z >= lay.z_min - 0.01
                    && wall.z <= lay.z_max + 0.01
            );
            assert!(wall.rot_y.to_degrees() >= -16.1);
            assert!(wall.rot_y.to_degrees() <= 14.1);

            let flat = card_pose(&lay, i, 0.0);
            assert_eq!(flat.z, 0.0);
            assert_eq!(flat.rot_y, 0.0);
            assert_eq!(flat.rot_x, 0.0);
            assert_eq!(flat.dim, 1.0);
            // 平面位置不随塌回改变:同一批卡只是深度插值。
            assert_eq!(flat.x, wall.x);
            assert_eq!(flat.y, wall.y);
        }
    }

    /// 松手后惯性衰减并吸附到列距整数倍,最终必须停(省电门依据)。
    #[test]
    fn released_camera_snaps_and_freezes() {
        let lay = wide();
        let mut cam = WallCam::default();
        cam.drag(-90.0, 0.0);
        assert!(cam.step(lay.col_pitch));
        cam.release();
        for _ in 0..300 {
            cam.step(lay.col_pitch);
        }
        assert!(
            !cam.step(lay.col_pitch),
            "300 帧后还在动,吸附不收敛"
        );
        let ratio = cam.pan_x / lay.col_pitch;
        assert!(
            (ratio - ratio.round()).abs() < 0.01,
            "没吸到格点:pan_x = {}",
            cam.pan_x
        );
        assert_eq!(cam.yaw, 0.0);
    }

    /// 滚轮推拉有行程限制,推不穿墙。
    #[test]
    fn wheel_dolly_is_clamped() {
        let mut cam = WallCam::default();
        for _ in 0..100 {
            cam.wheel(200.0);
        }
        assert!(cam.dolly <= 620.0);
        for _ in 0..100 {
            cam.wheel(-200.0);
        }
        assert!(cam.dolly >= -260.0);
    }

    /// 命中测试:把某张卡投到屏幕,再拿投影中心去问,要拿回同一张。
    #[test]
    fn hit_test_finds_the_projected_card() {
        let lay = wide();
        let cam = WallCam::default();
        let poses: Vec<CardPose> = (0..12)
            .map(|i| card_pose(&lay, i, 1.0))
            .collect();
        let world = world_pose(&cam, &poses[4]);
        let (sx, sy, _) =
            project(&lay, &cam, &world).unwrap();
        let hit =
            hit_test(&lay, &cam, &poses, sx, sy).unwrap();
        // 命中的要么是它自己,要么是遮在它前面(z 更大)的卡。
        assert!(
            world_pose(&cam, &poses[hit]).z >= world.z
        );
    }

    /// 空白处点不中任何卡。
    #[test]
    fn hit_test_misses_empty_space() {
        let lay = wide();
        let cam = WallCam::default();
        let poses: Vec<CardPose> = (0..6)
            .map(|i| card_pose(&lay, i, 1.0))
            .collect();
        assert_eq!(
            hit_test(&lay, &cam, &poses, 1.0, 1.0),
            None
        );
    }

    /// 塌回与 dolly 都在有限帧内收敛(420ms ≈ 25 帧的量级)。
    #[test]
    fn transitions_settle_within_a_second() {
        let mut c = Collapse::default();
        c.target = 0.0;
        let mut frames = 0;
        while c.step() {
            frames += 1;
            assert!(frames < 60, "塌回一秒都收不住");
        }

        let mut d =
            DollyRun { t: 0.0, target_z: 40.0 };
        let mut frames = 0;
        while !d.step() {
            frames += 1;
            assert!(frames < 60, "dolly 一秒都落不了位");
        }
    }

    /// 圆角烘焙:角上透明、中心不动、非角边缘不动。
    #[test]
    fn rounded_corners_are_baked_into_alpha() {
        let (w, h) = (32u32, 32u32);
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        bake_rounded(&mut rgba, w, h);
        let alpha = |x: u32, y: u32| {
            rgba[((y * w + x) * 4 + 3) as usize]
        };
        assert_eq!(alpha(0, 0), 0, "左上角该抠掉");
        assert_eq!(alpha(31, 31), 0, "右下角该抠掉");
        assert_eq!(alpha(16, 16), 255, "中心不许动");
        assert_eq!(alpha(16, 0), 255, "上边中点不在角区");
    }
}
