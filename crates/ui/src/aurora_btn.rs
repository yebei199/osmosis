//! 光带按钮的边界逻辑(docs/design/handoff-shaders.md §9/§10)。
//!
//! 视觉由 render3d 的 `AuroraBtnPass` 画;这里管两件事:设置开关的存取与
//! hover 振幅的收敛。渲染循环前台恒满帧(见 change_log 2026-08-11
//! always-on-rendering),按钮每帧重渲,不再有冻结门。
//!
//! 当前接了五槽:Home 空槽(nebula)、空状态「换一批推荐」(ribbon 绿板)、
//! 正在播放胶囊(fluid,#68)、播放页主控条底(fluid,缓冲时换 progress)
//! 与播放页两颗次要圆钮共用的一张 glass 底(#69)。后三槽按需追加,
//! 场区没量出尺寸或覆层不在场就整个不进合批。
//! 加按钮就是往 `lib.rs` 的驱动块里加一份 [`ButtonAnim`] 与一组 slint 属性。

use slint::ComponentHandle;

use crate::MainWindow;

/// 静息振幅:约一成亮度。悬停收敛到 1.0。
pub const REST_AMP: f32 = 0.12;

// 变体编号。着色器一条管线按 `uVariant` 分支,数学与用途见
// handoff-shaders.md §10;这里给它们名字,免得驱动块里散着裸数字。
/// 一次性高光,正弦流场中心线加逐通道色散。
pub const VARIANT_RIBBON: f32 = 0.0;
/// 域扭曲 fbm 加星点格,开销最高,一屏一颗。
pub const VARIANT_NEBULA: f32 = 1.0;
/// 域扭曲加 plume,`reveal` 沿 x 开闸保住左侧文字区。长条与胶囊用它。
pub const VARIANT_FLUID: f32 = 2.0;
/// 低密度底加折射亮边,开销最低,次要按钮可整屏铺。
pub const VARIANT_GLASS: f32 = 3.0;
/// fluid 加进度填充遮罩,交界处呼吸亮边。
pub const VARIANT_PROGRESS: f32 = 4.0;
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

/// 主控条底这一帧走哪个变体、喂多少进度。
///
/// 播放页的进度是播放键那个环,条上没有常驻细线可挂,所以 progress 变体
/// 只在缓冲时接管条底:那道呼吸亮边就是「还在动」的信号(#69)。
/// 比例来自外部,夹到 0..=1 再进着色器 —— NaN 会让填充遮罩整条翻掉。
pub fn fluid_or_progress(
    buffering: bool,
    ratio: f32,
) -> (f32, f32) {
    if !buffering {
        return (VARIANT_FLUID, 0.0);
    }
    let p =
        if ratio.is_finite() { ratio.clamp(0.0, 1.0) } else { 0.0 };
    (VARIANT_PROGRESS, p)
}

/// 侧栏底部那两颗圆钮的一槽(#71)。尺寸与 widgets.slint 的 `RoundControl`
/// 默认直径一致,改那边要同步这里。
///
/// 选中那颗把振幅拉满,另一颗停在低位:水滴的轨道不覆盖这两格,亮度就是
/// 它们表达「我被选中」的全部手段。两颗共用一张图会连这点差别都没有,
/// 所以各渲各的 —— 44×44 一槽,代价可以忽略。
pub fn nav_key_slot(
    scale: f32,
    seed: f32,
    selected: bool,
    colors: [[f32; 3]; 4],
) -> AuroraBtnSlotControls {
    AuroraBtnSlotControls {
        w: 44.0 * scale,
        h: 44.0 * scale,
        radius: 22.0 * scale,
        seed,
        speed: 0.7,
        amp: if selected { 1.0 } else { 0.35 },
        mode: 1.0,
        bands: 3.0,
        variant: VARIANT_GLASS,
        progress: 0.0,
        pointer: (0.5, 0.5),
        colors,
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

    /// 缓冲时主控条底从 fluid 切成 progress、进度实时喂;不缓冲时切回
    /// fluid 且 progress 归零(变体数学见 handoff-shaders.md §10)。
    #[test]
    fn the_bar_switches_to_progress_while_buffering() {
        assert_eq!(
            fluid_or_progress(true, 0.4),
            (VARIANT_PROGRESS, 0.4),
            "缓冲时该走 progress 变体并喂进度"
        );
        assert_eq!(
            fluid_or_progress(false, 0.4),
            (VARIANT_FLUID, 0.0),
            "不缓冲时该切回 fluid,进度归零"
        );
    }

    /// 比例喂进去先夹到 0..=1:进度是外来数据,越界或非有限值会让
    /// 填充遮罩翻到条外。
    #[test]
    fn the_progress_ratio_is_clamped_before_it_reaches_the_shader(
    ) {
        assert_eq!(fluid_or_progress(true, 1.8).1, 1.0);
        assert_eq!(fluid_or_progress(true, -0.3).1, 0.0);
        assert_eq!(
            fluid_or_progress(true, f32::NAN).1,
            0.0,
            "非有限值该按 0 处理,不能带着 NaN 进着色器"
        );
    }

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
