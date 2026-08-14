//! 卡墙相机:拖拽惯性、吸附格点、推拉与俯仰,以及世界位姿到视口的投影。

use super::*;

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

/// 拖动惯性的阻尼(handoff 交互表)。
pub(super) const DRAG_DAMPING: f32 = 0.92;

/// 松手后向吸附点靠拢的每帧比例。
pub(super) const SNAP_CONVERGE: f32 = 0.14;

/// yaw 随拖动速度的比例与上限(±14°,别把墙拖成侧面)。
pub(super) const YAW_PER_VEL: f32 = 0.0016;

pub(super) const YAW_MAX: f32 = 0.244;

/// dolly 行程限制:太近穿卡,太远全糊。
pub(super) const DOLLY_MIN: f32 = -260.0;

pub(super) const DOLLY_MAX: f32 = 620.0;

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

impl WallCam {
    /// 指针拖动一帧的位移(物理像素)。
    pub fn drag(&mut self, dx: f32, dy: f32) {
        self.dragging = true;
        self.pan_x -= dx;
        self.vel_x = -dx;
        self.pitch =
            (self.pitch - dy * 0.0009).clamp(-0.12, 0.12);
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
        self.pan_x += (snap - self.pan_x) * SNAP_CONVERGE;
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
pub const BASE_PITCH: f32 =
    6.0 * core::f32::consts::PI / 180.0;

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

