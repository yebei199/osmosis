//! 卡墙的两段动画:双击进播放页的相机 dolly,以及列表 ⇄ 卡墙的塌陷。

use super::*;

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
        Self {
            value: 1.0,
            target: 1.0,
        }
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
