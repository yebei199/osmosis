//! 光带按钮的边界逻辑(docs/design/handoff-shaders.md §9/§10)。
//!
//! 视觉由 render3d 的 `AuroraBtnPass` 画;这里管两件事:设置开关的存取与
//! hover 振幅的收敛。渲染循环前台恒满帧(见 change_log 2026-08-11
//! always-on-rendering),按钮每帧重渲,不再有冻结门。
//!
//! 当前接了两颗:Home 空槽(nebula)与空状态「换一批推荐」(ribbon 绿板)。
//! 加按钮就是往 `lib.rs` 的驱动块里加一份 [`ButtonAnim`] 与一组 slint 属性。

use slint::ComponentHandle;

use crate::MainWindow;

/// 静息振幅:约一成亮度。悬停收敛到 1.0。
pub const REST_AMP: f32 = 0.12;
/// 每帧向目标靠拢的比例(与参考实现同值,约 90ms 到位)。
const CONVERGE: f32 = 0.09;

/// 一颗按钮这一帧的控制量,**物理像素**。POD,apps/* 在 seam 处平凡拷成
/// `render3d::AuroraBtnSlot`(镜像分离,ui 与 render3d 互不依赖)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuroraBtnSlotControls {
    pub w: f32,
    pub h: f32,
    pub radius: f32,
    pub seed: f32,
    pub speed: f32,
    pub amp: f32,
    pub mode: f32,
    pub bands: f32,
    pub variant: f32,
    pub progress: f32,
    pub pointer: (f32, f32),
    pub colors: [[f32; 3]; 4],
}

/// 一帧全部按钮的控制量。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AuroraBtnControls {
    /// 按钮时钟,秒。每帧推进。
    pub time: f32,
    pub slots: Vec<AuroraBtnSlotControls>,
}

/// 一颗按钮的跨帧动画状态:振幅与指针都朝目标收敛。
#[derive(Clone, Copy, Debug)]
pub struct ButtonAnim {
    pub amp: f32,
    pub px: f32,
    pub py: f32,
}

impl Default for ButtonAnim {
    fn default() -> Self {
        Self { amp: REST_AMP, px: 0.72, py: 0.5 }
    }
}

impl ButtonAnim {
    /// 振幅与指针朝目标走一步。收敛与否不再有人问:循环恒满帧,每帧都渲。
    pub fn step(
        &mut self,
        hovered: bool,
        pointer: (f32, f32),
    ) {
        let target = if hovered { 1.0 } else { REST_AMP };
        let (ptx, pty) =
            if hovered { pointer } else { (0.72, 0.5) };
        self.amp += (target - self.amp) * CONVERGE;
        self.px += (ptx - self.px) * 0.10;
        self.py += (pty - self.py) * 0.10;
    }
}

/// 恢复开关并接上设置页的拨动。值的真相在 `api::settings`,跟设备走。
pub(crate) fn bind(ui: &MainWindow) {
    ui.set_aurora_buttons_on(
        api::settings::load().aurora_buttons,
    );

    let weak = ui.as_weak();
    ui.on_aurora_buttons_toggled(move || {
        let Some(ui) = weak.upgrade() else { return };
        let on = !ui.get_aurora_buttons_on();
        ui.set_aurora_buttons_on(on);
        api::settings::save(&api::settings::Settings {
            aurora_buttons: on,
            ..api::settings::load()
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 悬停把振幅拉向 1,离开收回静息:冻结门撤了,收敛数学原样保留。
    #[test]
    fn hover_heats_up_and_leave_cools_down() {
        let mut anim = ButtonAnim::default();

        for _ in 0..200 {
            anim.step(true, (0.6, 0.4));
        }
        assert!(
            (anim.amp - 1.0).abs() < 0.01,
            "悬停该收敛到满幅,实得 {}",
            anim.amp
        );

        for _ in 0..300 {
            anim.step(false, (0.6, 0.4));
        }
        assert!(
            (anim.amp - REST_AMP).abs() < 0.01,
            "该回到静息振幅,实得 {}",
            anim.amp
        );
    }
}
