//! 封面实物的运动学:呼吸、拖动回弹、换歌那一下的交接,以及台面随视口的尺寸。
//!
//! 全是纯函数与小状态机,不碰 bevy 的 ECS —— 角度、进度与尺寸在这里算好,
//! `Sleeve::drive` 只把它们写进 `Transform` 与材质。时间一律按播放页时钟走
//! (门关即冻结),帧率变了快慢不变。

use std::f32::consts::PI;

use bevy::prelude::*;

use crate::camera::{BASE_CAMERA_POS, BASE_FOV};

// ── 呼吸 ──────────────────────────────────────────────────────────────

/// 呼吸的左右摆幅,约 ±3°。再大就不是「放在那儿的一个东西」,而是在晃。
const IDLE_YAW: f32 = PI / 60.0;

/// 呼吸的前后摆幅,约 ±2°。比左右小一档:前后倾会让顶边忽大忽小,同样的角度更显眼。
const IDLE_PITCH: f32 = PI / 90.0;

/// 两路的周期,秒。互质得够远,合起来的轨迹要好几分钟才重复一次,看不出是循环。
const IDLE_YAW_PERIOD: f32 = 11.0;
const IDLE_PITCH_PERIOD: f32 = 15.0;

/// 这一刻的呼吸姿态 (pitch, yaw),弧度。
///
/// 只跟时间走,不跟声音走:「随音乐」只经光(见 `sleeve.wgsl` 的边缘光),
/// 几何一动不动 —— 一张封套跟着鼓点抖,看着像坏了而不是在听歌。
pub(crate) fn idle(time: f32) -> (f32, f32) {
    let yaw = IDLE_YAW
        * (2.0 * PI * time / IDLE_YAW_PERIOD).sin();
    let pitch = IDLE_PITCH
        * (2.0 * PI * time / IDLE_PITCH_PERIOD + 1.0).sin();
    (pitch, yaw)
}

// ── 拖动与回弹 ────────────────────────────────────────────────────────

/// 拖一个物理像素转多少弧度,与点云的拖动同一档手感。
const TILT_PER_PIXEL: f32 = 0.0034;

/// 拖动的最大偏角(左右约 40°、前后约 25°),越往边上越拖不动(橡皮筋)。
///
/// 拿在手里看一张封套,手腕转不过这个角度;再往后就是在看它的背面了,
/// 而背面没印东西。
pub(crate) const MAX_YAW: f32 = 2.0 * PI / 9.0;
pub(crate) const MAX_PITCH: f32 = 5.0 * PI / 36.0;

/// 回弹的弹簧:刚度与阻尼比。阻尼比小于 1,松手时冲过头一点再停 ——
/// 那一下是「放回去」的手感;临界阻尼会像被吸回去。
const SPRING_STIFFNESS: f32 = 70.0;
const SPRING_DAMPING_RATIO: f32 = 0.55;

/// 松手时带出去的角速度上限,rad/s。甩得再快也不该转出一圈。
const MAX_RATE: f32 = 4.0;

/// 弹簧积分的最大步长,秒。帧慢的时候切成几小步,不然大步长的显式积分会炸。
const MAX_STEP: f32 = 1.0 / 120.0;

/// 拖动带来的偏转,松手后由弹簧拉回原位。
///
/// 与点云的 `Spin` 不同:那边松手后靠惯性一直滑,转到哪儿停哪儿;封套是拿在手里
/// 翻看的一个东西,松手就回到它本来的位置。
#[derive(Default)]
pub(crate) struct Tilt {
    pitch: f32,
    yaw: f32,
    pitch_rate: f32,
    yaw_rate: f32,
    held: bool,
}

impl Tilt {
    /// 拖了 `(dx, dy)` 个物理像素,用时 `dt` 秒。横向转 yaw、纵向转 pitch。
    ///
    /// 偏角走橡皮筋:原始拖动量线性累加,显示出来的角度按 `tanh` 收向上限。
    /// 每次从当前角度反推回原始量再加 —— 回弹到一半又按住,不会跳。
    pub(crate) fn drag(
        &mut self,
        dx: f32,
        dy: f32,
        dt: f32,
    ) {
        self.held = true;
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        let pitch = rubber(
            unrubber(self.pitch, MAX_PITCH)
                + dy * TILT_PER_PIXEL,
            MAX_PITCH,
        );
        let yaw = rubber(
            unrubber(self.yaw, MAX_YAW)
                + dx * TILT_PER_PIXEL,
            MAX_YAW,
        );
        if dt.is_finite() && dt > 0.0 {
            self.pitch_rate = ((pitch - self.pitch) / dt)
                .clamp(-MAX_RATE, MAX_RATE);
            self.yaw_rate = ((yaw - self.yaw) / dt)
                .clamp(-MAX_RATE, MAX_RATE);
        }
        self.pitch = pitch;
        self.yaw = yaw;
    }

    /// 松手(或指针离开):从下一帧起由弹簧接管,带着最后那一下的角速度。
    pub(crate) fn release(&mut self) {
        self.held = false;
    }

    /// 推进 `dt` 秒。按着的时候不动(角度由拖动直接给);坏 `dt` 当作没走。
    pub(crate) fn step(&mut self, dt: f32) {
        if self.held || !dt.is_finite() || dt <= 0.0 {
            return;
        }
        let damping = 2.0
            * SPRING_DAMPING_RATIO
            * SPRING_STIFFNESS.sqrt();
        let mut left = dt;
        while left > 0.0 {
            let h = left.min(MAX_STEP);
            left -= h;
            for (angle, rate, limit) in [
                (
                    &mut self.pitch,
                    &mut self.pitch_rate,
                    MAX_PITCH,
                ),
                (
                    &mut self.yaw,
                    &mut self.yaw_rate,
                    MAX_YAW,
                ),
            ] {
                // 半隐式欧拉:先更新速度再用新速度推位置,弹簧不会越积越有劲。
                *rate += (-SPRING_STIFFNESS * *angle
                    - damping * *rate)
                    * h;
                *angle += *rate * h;
                // 甩出去的那一下也不许越过拖动的上限:撞到就停在那儿,由弹簧拉回。
                if angle.abs() > limit {
                    *angle = angle.clamp(-limit, limit);
                    *rate = 0.0;
                }
            }
        }
        // 回到看不出的范围就归零,免得一份永远非零的残差每帧标脏。
        for (angle, rate) in [
            (&mut self.pitch, &mut self.pitch_rate),
            (&mut self.yaw, &mut self.yaw_rate),
        ] {
            if angle.abs() < 1e-4 && rate.abs() < 1e-3 {
                *angle = 0.0;
                *rate = 0.0;
            }
        }
    }

    /// 当前偏角 (pitch, yaw),弧度。
    pub(crate) fn angles(&self) -> (f32, f32) {
        (self.pitch, self.yaw)
    }
}

/// 橡皮筋:原始量线性涨,显示量按 `tanh` 收向 `±limit`。
fn rubber(raw: f32, limit: f32) -> f32 {
    limit * (raw / limit).tanh()
}

/// [`rubber`] 的反函数。贴着上限时夹一下,`atanh(±1)` 是无穷。
fn unrubber(shown: f32, limit: f32) -> f32 {
    limit * (shown / limit).clamp(-0.999, 0.999).atanh()
}

// ── 换歌的交接 ────────────────────────────────────────────────────────

/// 交接一次多久,秒。比点云的颜色渐变(0.9s)短一点:实物的运动读得快,
/// 拖长了像慢动作。
pub(crate) const EXCHANGE_SECS: f32 = 0.8;

/// 换歌时旧封套退场、新封套落位的那一段计时。
pub(crate) struct Exchange {
    /// 距上次交接开始的秒数。初值取一个已经跑完的值:首曲没有可退场的旧封套。
    elapsed: f32,
}

impl Default for Exchange {
    fn default() -> Self {
        Self {
            elapsed: EXCHANGE_SECS,
        }
    }
}

impl Exchange {
    pub(crate) fn start(&mut self) {
        self.elapsed = 0.0;
    }

    /// 直接走完。
    pub(crate) fn finish(&mut self) {
        self.elapsed = EXCHANGE_SECS;
    }

    /// 推进 `dt` 秒,计满即停。坏 `dt` 当作没走 —— 过渡不能倒着走。
    pub(crate) fn advance(&mut self, dt: f32) {
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        self.elapsed =
            (self.elapsed + dt).min(EXCHANGE_SECS);
    }

    /// 进度 0..=1。
    pub(crate) fn progress(&self) -> f32 {
        (self.elapsed / EXCHANGE_SECS).clamp(0.0, 1.0)
    }

    /// 退场的那一张还在画面里。
    pub(crate) fn running(&self) -> bool {
        self.elapsed < EXCHANGE_SECS
    }
}

/// 一张封套相对它静止位置的偏移:平移(台面单位)与三个角(弧度)。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Offset {
    pub translation: Vec3,
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
}

/// 新封套在进度 `e` 时的偏移:从后上方往前落,冲过头一点再停住;`e = 1` 时归零。
///
/// 起点在旧封套**后面**(z 更小):旧的被拨开,露出后面那张,后面那张再往前落位 ——
/// 两张始终不互相穿过,不必靠透明度遮丑(安卓上也没有透明度可用,见 `sleeve.rs`)。
pub(crate) fn incoming(e: f32) -> Offset {
    let left = 1.0 - ease_out_back(e.clamp(0.0, 1.0));
    Offset {
        translation: Vec3::new(0.0, 0.10, -0.9) * left,
        pitch: -0.30 * left,
        yaw: 0.25 * left,
        roll: 0.0,
    }
}

/// 旧封套在进度 `e` 时的偏移:加速往左后方甩出去,同时翻向一侧;`e = 0` 时归零。
pub(crate) fn outgoing(e: f32) -> Offset {
    let e = e.clamp(0.0, 1.0);
    let k = e * e;
    Offset {
        translation: Vec3::new(-1.35, -0.05, -0.6) * k,
        pitch: 0.0,
        yaw: -1.25 * k,
        roll: 0.12 * k,
    }
}

/// 冲过头再回来的缓出(Penner 的 easeOutBack,过冲量取得比原版小)。
/// `ease_out_back(0) = 0`,`ease_out_back(1) = 1`,中途略超过 1。
fn ease_out_back(x: f32) -> f32 {
    const C1: f32 = 1.2;
    const C3: f32 = C1 + 1.0;
    let t = x - 1.0;
    1.0 + C3 * t * t * t + C1 * t * t
}

// ── 光与显影 ──────────────────────────────────────────────────────────

/// 边缘光跟低频的快慢:起得快、落得慢(1/秒)。起得慢就跟不上鼓点,
/// 落得快就是一闪一闪的,不是「被照亮」。
const GLOW_ATTACK: f32 = 14.0;
const GLOW_RELEASE: f32 = 4.0;

/// 平滑过的低频电平,点亮封套边缘那一圈光。
#[derive(Default)]
pub(crate) struct Glow {
    level: f32,
}

impl Glow {
    pub(crate) fn follow(&mut self, target: f32, dt: f32) {
        if !dt.is_finite()
            || dt <= 0.0
            || !target.is_finite()
        {
            return;
        }
        let rate = if target > self.level {
            GLOW_ATTACK
        } else {
            GLOW_RELEASE
        };
        self.level += (target.clamp(0.0, 1.0) - self.level)
            * (1.0 - (-rate * dt).exp());
    }

    pub(crate) fn level(&self) -> f32 {
        self.level
    }
}

/// 封面印上空封套要多久,秒。
const REVEAL_SECS: f32 = 0.35;

/// 封面在封套正面上的显影程度:0 是空封套,1 是印满。
///
/// 只用于「空封套上印出封面」那一种情况(先换歌、几百毫秒后封面才到)。
/// 正在显示一张封面时又来一张,那是一次交接,新的那张直接印满(见 `Sleeve::show`)。
#[derive(Default)]
pub(crate) struct Reveal {
    value: f32,
    target: f32,
}

impl Reveal {
    /// 立刻到位,不过渡。
    pub(crate) fn snap(&mut self, value: f32) {
        self.value = value;
        self.target = value;
    }

    /// 从当前值匀速走向 `target`。
    pub(crate) fn fade_to(&mut self, target: f32) {
        self.target = target;
    }

    /// 直接到目标值。
    pub(crate) fn settle(&mut self) {
        self.value = self.target;
    }

    pub(crate) fn advance(&mut self, dt: f32) {
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        let step = dt / REVEAL_SECS;
        self.value = if self.value < self.target {
            (self.value + step).min(self.target)
        } else {
            (self.value - step).max(self.target)
        };
    }

    pub(crate) fn value(&self) -> f32 {
        self.value
    }

    /// 正面这会儿印着(或正在印上)一张封面。
    pub(crate) fn printed(&self) -> bool {
        self.target > 0.5
    }
}

// ── 台面 ──────────────────────────────────────────────────────────────

/// 台面往前倾的角度(约 9°):等价于相机略微俯视,看得见封套的顶边和身前那片地面。
///
/// 不挪相机:点云要正对着看,两个效果共用一台相机(见 `camera::BASE_CAMERA_POS`)。
pub(crate) const STAGE_PITCH: f32 = PI / 20.0;

/// 封套中心比主轴高出多少,以可视高度为单位。往上让一点,给下面的倒影留地方。
const STAGE_LIFT: f32 = 0.05;

/// 封套的边长最多占可视高度多少。
const FIT_HEIGHT: f32 = 0.46;

/// 封套的边长最多占可视宽度多少:宽版式右边四成是歌名与歌词,只剩左边那一块;
/// 紧凑版式左右都空着,占得多一些。
const FIT_WIDTH_WIDE: f32 = 0.48;
const FIT_WIDTH_COMPACT: f32 = 0.70;

/// 台面原点所在深度上,画面纵向看得见多少世界单位。
///
/// 移轴不改这个数:镜头只平移、不缩放(见 `camera::lens`)。
fn visible_height() -> f32 {
    2.0 * BASE_CAMERA_POS.z * (BASE_FOV * 0.5).tan()
}

/// 台面的缩放:一个台面单位(封套边长)合多少世界单位。
///
/// 取「高度够放」与「宽度够放」里小的那个。0 尺寸(首帧、刚重建)按高度算。
pub(crate) fn stage_scale(
    width: u32,
    height: u32,
    compact: bool,
) -> f32 {
    let tall = visible_height();
    let by_height = FIT_HEIGHT * tall;
    if width == 0 || height == 0 {
        return by_height;
    }
    let wide = tall * width as f32 / height as f32;
    let fit = if compact {
        FIT_WIDTH_COMPACT
    } else {
        FIT_WIDTH_WIDE
    };
    by_height.min(fit * wide)
}

/// 台面原点在世界里的高度:主轴之上一点(见 [`STAGE_LIFT`])。
pub(crate) fn stage_lift() -> f32 {
    STAGE_LIFT * visible_height()
}

#[cfg(test)]
mod tests;
