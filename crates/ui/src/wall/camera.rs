//! 卡墙相机:双轴拖拽惯性、环面回绕,以及世界位姿到视口的投影。

use super::*;

/// 卡墙相机:任意方向拖动平移(带惯性),拖动速度顺带把整墙微微转一下。
///
/// 全部按帧收敛(与 `aurora_btn::ButtonAnim` 同风格,不带 dt):
/// 60fps 下惯性阻尼 0.92 的手感即设计稿标定值。
#[derive(Clone, Copy, Debug, Default)]
pub struct WallCam {
    pub pan_x: f32,
    pub pan_y: f32,
    pub yaw: f32,
    pub pitch: f32,
    vel_x: f32,
    vel_y: f32,
    dragging: bool,
}

/// 拖动惯性的阻尼(handoff 交互表)。
pub(super) const DRAG_DAMPING: f32 = 0.92;

/// 速度的跨帧平滑系数:新的一帧占多少权重。
///
/// **不能直接取「这一帧的指针位移」当速度。** 指针位置只在 `moved` 事件里
/// 镜像过来,而循环是每帧跑的:鼠标上报率低于帧率时,有的帧根本没有新事件,
/// 位移是 0。姿态角由速度直接算的话就会在满偏与归零之间逐帧来回跳,整墙看着
/// 在抖;松手前那一帧若恰好没事件,惯性还会整个丢掉。
///
/// 按帧做指数滑动平均之后,速度收敛到**每帧平均位移** —— 而滑行正是每帧
/// 加一个 `vel`,两者同一个量纲,高刷屏上也不会因为空帧变多而滑得更短。
pub(super) const VEL_SMOOTHING: f32 = 0.3;

/// yaw 随拖动速度的比例与上限(±14°,别把墙拖成侧面)。
pub(super) const YAW_PER_VEL: f32 = 0.0016;

pub(super) const YAW_MAX: f32 = 0.244;

/// pitch 随竖向拖动速度的比例与上限。比 yaw 收得更紧:整墙本来就带
/// 6° 基础俯仰,再多就露出卡的底面。
pub(super) const PITCH_PER_VEL: f32 = 0.0009;

pub(super) const PITCH_MAX: f32 = 0.12;

impl WallCam {
    /// 指针拖动一帧的位移(物理像素)。两轴都平移 —— 卡墙是张可以往
    /// 任意方向拖的地图,不锁轴。
    pub fn drag(&mut self, dx: f32, dy: f32) {
        self.dragging = true;
        // 平移吃满这一帧的位移(它必须逐事件精确跟手),
        // 速度只取平滑值(它喂的是姿态角与惯性,要的是稳)。
        self.pan_x -= dx;
        self.pan_y -= dy;
        self.blend_velocity(-dx, -dy);
    }

    /// 把一帧的位移并进平滑速度。见 [`VEL_SMOOTHING`]。
    fn blend_velocity(&mut self, vx: f32, vy: f32) {
        let k = VEL_SMOOTHING;
        self.vel_x += (vx - self.vel_x) * k;
        self.vel_y += (vy - self.vel_y) * k;
    }

    pub fn release(&mut self) {
        self.dragging = false;
    }

    /// 滚轮:桌面上就是「竖着划一下」,与移动端同一套语义。
    /// 推拉镜头不再挂在滚轮上 —— 那一档只剩双击进播放页时的自动 dolly。
    pub fn wheel(&mut self, delta: f32) {
        self.pan_y -= delta;
        // 给一点滑行,滚轮连打时才不像一格一格地跳。
        self.blend_velocity(self.vel_x, -delta);
    }

    /// 走一帧,返回**还在动吗**(动 = 这一帧要重渲)。
    pub fn step(&mut self) -> bool {
        // 姿态跟着这一帧的速度偏:拖动中是手感,松手后随惯性一起衰减。
        self.yaw = (self.vel_x * YAW_PER_VEL * 60.0)
            .clamp(-YAW_MAX, YAW_MAX);
        self.pitch = (self.vel_y * PITCH_PER_VEL * 60.0)
            .clamp(-PITCH_MAX, PITCH_MAX);
        if self.dragging {
            return true;
        }
        // 惯性滑行。不吸附格点:环面上没有「一屏」的概念,停在哪都成立。
        self.vel_x *= DRAG_DAMPING;
        self.vel_y *= DRAG_DAMPING;
        self.pan_x += self.vel_x;
        self.pan_y += self.vel_y;

        let moving = self.vel_x.abs() > SETTLED
            || self.vel_y.abs() > SETTLED;
        if !moving {
            self.vel_x = 0.0;
            self.vel_y = 0.0;
            self.yaw = 0.0;
            self.pitch = 0.0;
        }
        moving
    }
}

/// 相机相对场心下移多少(占场高的比例)。透视原点 42% 的效果由它实现,
/// render3d 的相机严格同构。
pub const CAM_Y_RATIO: f32 = 0.08;

/// 卡片静息时的基础后倒(CSS `rotateX(6deg)`):每张卡自己微微向后仰。
pub const BASE_PITCH: f32 =
    6.0 * core::f32::consts::PI / 180.0;

/// 把 `v` 折回以 0 为中心、周期 `span` 的区间 `[-span/2, span/2)`。
fn wrap(v: f32, span: f32) -> f32 {
    if span <= 0.0 {
        return v;
    }
    (v + span * 0.5).rem_euclid(span) - span * 0.5
}

/// 把网格位姿变换到世界:按相机平移**回绕到环面上离镜头最近的那一格**,
/// 再把整场姿态角(基础后倒 + 用户拖出的 yaw/pitch)并进卡片自身的朝向。
///
/// **姿态角只转卡片,不转卡片的位置。** 曾经是绕镜头旋转整个网格 ——
/// 那样 yaw 会把 x 混进 z(`z1 = -gx·sin(yaw) + z·cos(yaw)`),于是 gx 回绕
/// 一个周期时深度跟着跳 `span_x·sin(yaw)`,投影缩放突变,卡片带着尺寸变化
/// 凭空出现在屏幕里。环面的前提是「回绕在屏幕上什么也没发生」,而只要整墙
/// 在转,这个前提就不成立;边距开多大都补不回来,因为跳的是深度不是位置。
/// 改成只转朝向之后,回绕是纯粹的周期平移,深度一动不动。
///
/// **这是 ui 与 render3d 之间唯一的变换真相**:render3d 只把结果摆进场景,
/// 相机不带任何旋转也不平移,命中测试与渲染因此天然一致。
pub fn world_pose(
    lay: &WallLayout,
    cam: &WallCam,
    pose: &CardPose,
) -> CardPose {
    // 竖向绕**相机看的那个中心**回绕,不是绕场心:相机为了透视原点下移了
    // 8% 场高,窗口不跟着挪的话上下两边的余量就不对等,矮的那边会在屏内回绕。
    let cy = CAM_Y_RATIO * lay.h;
    CardPose {
        x: wrap(pose.x - cam.pan_x, lay.span_x()),
        y: wrap(pose.y - cam.pan_y - cy, lay.span_y()) + cy,
        z: pose.z,
        rot_y: pose.rot_y + cam.yaw,
        rot_x: pose.rot_x + BASE_PITCH + cam.pitch,
        dim: pose.dim,
    }
}

/// 把一张**世界位姿**的卡投到屏幕:返回(中心 x、中心 y、半边长),
/// 贴到近平面上给 None。相机在 (0, 0.08h) 处、距 z=0 平面 `perspective`,
/// 视轴不带旋转 —— 透视原点 42% 的效果由相机下移 8% 实现,render3d 的
/// 相机严格同构。平移已经在 [`world_pose`] 的回绕里减掉了。
pub fn project(
    lay: &WallLayout,
    world: &CardPose,
) -> Option<(f32, f32, f32)> {
    let d = lay.perspective;
    let dist = d - world.z;
    if dist <= 1.0 {
        return None;
    }
    let s = d / dist;
    let cam_y = CAM_Y_RATIO * lay.h;
    Some((
        lay.w * 0.5 + world.x * s,
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
        let world = world_pose(lay, cam, pose);
        let Some((sx, sy, half)) = project(lay, &world)
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
