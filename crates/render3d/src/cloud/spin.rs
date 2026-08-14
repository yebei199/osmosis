//! 拖动旋转的惯性:拖时累积角速度,松手后按阻尼滑行到静止。

/// 横拖一像素转多少弧度(绕 Y),同原版 `PARTICLE_POINTER_SPIN_Y`。
const SPIN_PER_PIXEL_Y: f32 = 0.0034;

/// 纵拖一像素转多少弧度(绕 X),同原版 `PARTICLE_POINTER_SPIN_X`。
const SPIN_PER_PIXEL_X: f32 = 0.0032;

/// 角速度上限,rad/s。同原版 `PARTICLE_SPIN_MAX`。
const SPIN_MAX: f32 = 6.2;

/// 松手时把「这一段拖了多少」换算成角速度的系数,同原版 `applyParticleSpinDrag`。
const SPIN_RELEASE: f32 = 0.46;

/// 每帧留下多少角速度,同原版 `particleSpin.damping`。
const SPIN_DAMPING: f32 = 0.90;

/// 拖动带来的点云自转,外加松手后的惯性。
///
/// 转的是**点云自己**而不是相机 —— 原版就是把 `gestureRotation` 加到
/// `particles.rotation` 上。相机一动,遮挡层那台就得跟着动,两层还得逐像素对齐;
/// 转物体没有这个牵连。
#[derive(Default)]
pub(crate) struct Spin {
    /// 累计角度,弧度。
    pitch: f32,
    yaw: f32,
    /// 松手后的角速度,rad/s。
    pitch_rate: f32,
    yaw_rate: f32,
}

impl Spin {
    /// 拖了 `(dx, dy)` 个像素,`dt` 秒。横向转 yaw、纵向转 pitch。
    pub(crate) fn drag(
        &mut self,
        dx: f32,
        dy: f32,
        dt: f32,
    ) {
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        let d_pitch = dy * SPIN_PER_PIXEL_X;
        let d_yaw = dx * SPIN_PER_PIXEL_Y;
        self.pitch += d_pitch;
        self.yaw += d_yaw;
        if dt.is_finite() && dt > 0.0 {
            self.pitch_rate =
                clamp_rate(d_pitch / dt * SPIN_RELEASE);
            self.yaw_rate =
                clamp_rate(d_yaw / dt * SPIN_RELEASE);
        }
    }

    /// 松手后的惯性:按角速度继续转,同时衰减。坏 `dt` 当作没走。
    pub(crate) fn coast(&mut self, dt: f32) {
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        self.pitch += self.pitch_rate * dt;
        self.yaw += self.yaw_rate * dt;
        // 原版按帧衰减 0.90;这里按时间衰减,帧率一变角速度的衰减快慢才不跟着变。
        let decay = SPIN_DAMPING.powf(dt * 60.0);
        self.pitch_rate *= decay;
        self.yaw_rate *= decay;
        // 衰到看不出来就归零,免得留一份永远非零的状态每帧标脏。
        if self.pitch_rate.abs() < 1e-4 {
            self.pitch_rate = 0.0;
        }
        if self.yaw_rate.abs() < 1e-4 {
            self.yaw_rate = 0.0;
        }
    }

    /// 当前的累计角度 (pitch, yaw),弧度。
    pub(crate) fn angles(&self) -> (f32, f32) {
        (self.pitch, self.yaw)
    }
}

/// 角速度钳进 ±[`SPIN_MAX`];非有限值归零,同原版 `clampParticleSpinVelocity`。
fn clamp_rate(rate: f32) -> f32 {
    if rate.is_finite() {
        rate.clamp(-SPIN_MAX, SPIN_MAX)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests;
