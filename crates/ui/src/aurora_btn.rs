//! 光带按钮的边界逻辑(docs/design/handoff-shaders.md §9/§10)。
//!
//! 视觉由 render3d 的 `AuroraBtnPass` 画;这里管三件事:设置开关的存取、
//! hover 振幅的收敛,以及省电门 —— 判断这一帧要不要重渲按钮纹理。
//! 收敛完(静息或悬停到位)就冻结:时钟不走、纹理复用上一帧,
//! 静止画面零重绘(docs/design.md 硬规则 7)。
//!
//! 当前接了两颗:Home 空槽(nebula)与空状态「换一批推荐」(ribbon 绿板)。
//! 加按钮就是往 `lib.rs` 的驱动块里加一份 [`ButtonAnim`] 与一组 slint 属性。

use slint::ComponentHandle;

use crate::MainWindow;

/// 静息振幅:约一成亮度。悬停收敛到 1.0。
pub const REST_AMP: f32 = 0.12;
/// 每帧向目标靠拢的比例(与参考实现同值,约 90ms 到位)。
const CONVERGE: f32 = 0.09;
/// 与目标差距小于它就算收敛,可以冻结。
const SETTLED: f32 = 0.004;

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
    /// 按钮时钟,秒。只在有按钮未收敛时推进。
    pub time: f32,
    pub slots: Vec<AuroraBtnSlotControls>,
}

/// 一颗按钮的跨帧动画状态:振幅与指针都朝目标收敛。
#[derive(Clone, Copy, Debug)]
pub struct ButtonAnim {
    pub amp: f32,
    pub px: f32,
    pub py: f32,
    /// 冻结前还欠一帧:收敛后的最后一帧要渲出静止态。
    rendered_settled: bool,
}

impl Default for ButtonAnim {
    fn default() -> Self {
        Self {
            amp: REST_AMP,
            px: 0.72,
            py: 0.5,
            rendered_settled: false,
        }
    }
}

impl ButtonAnim {
    /// 朝目标走一步,返回**这一帧是否需要重渲**。
    ///
    /// 未收敛 → 渲;刚收敛的那一帧 → 再渲一次拿到静止态;之后 → 冻结,
    /// 直到 hover/指针再动。首帧(还没渲过静止态)也要渲一次,
    /// 不然开关刚打开时按钮是空的。
    pub fn step(
        &mut self,
        hovered: bool,
        pointer: (f32, f32),
    ) -> bool {
        let target = if hovered { 1.0 } else { REST_AMP };
        let (ptx, pty) =
            if hovered { pointer } else { (0.72, 0.5) };

        let hot = (target - self.amp).abs() > SETTLED
            || (ptx - self.px).abs() > SETTLED
            || (pty - self.py).abs() > SETTLED;
        if hot {
            self.amp += (target - self.amp) * CONVERGE;
            self.px += (ptx - self.px) * 0.10;
            self.py += (pty - self.py) * 0.10;
            self.rendered_settled = false;
            return true;
        }
        if !self.rendered_settled {
            // 收敛后的最后一帧:把静止态渲出来,然后冻结。
            self.rendered_settled = true;
            return true;
        }
        false
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

    /// 收敛后冻结:补渲一帧静止态,然后不再要求重渲。
    #[test]
    fn a_settled_button_freezes_after_one_last_frame() {
        let mut anim = ButtonAnim::default();

        // 初始就在静息位:首帧补渲静止态,之后冻结。
        assert!(anim.step(false, (0.5, 0.5)));
        assert!(!anim.step(false, (0.5, 0.5)));
        assert!(!anim.step(false, (0.5, 0.5)));
    }

    /// 悬停把振幅拉向 1,期间每帧都要渲;离开再收回静息,最终仍会冻结。
    #[test]
    fn hover_heats_up_and_leave_cools_down() {
        let mut anim = ButtonAnim::default();
        let _ = anim.step(false, (0.5, 0.5));
        assert!(!anim.step(false, (0.5, 0.5)), "先冻住");

        assert!(
            anim.step(true, (0.6, 0.4)),
            "悬停该立刻热起来"
        );
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
            !anim.step(false, (0.6, 0.4)),
            "离开后最终该再次冻结"
        );
        assert!(
            (anim.amp - REST_AMP).abs() < 0.01,
            "该回到静息振幅,实得 {}",
            anim.amp
        );
    }
}
