//! 导航液态玻璃选中器的边界逻辑(宽版式侧栏 / 紧凑版式底栏)。
//!
//! 视觉(metaball 融合、极光介质、边缘折射)由 render3d 的独立 wgpu pass + shader 画,
//! Slint 只出 hover/点击/选区与几何。选中块的 metaball 靠**三个动画位置**做出来:
//! `lead`(快)、`lag`(慢)与 `drop`(最慢)都朝当前选中槽中心移动,行走时前后拉开,
//! shader 对三个圆角矩形取 smooth-union 拉出胶着的颈;静止时重合成单块。
//! 两种版式共用这一套,差别只在移动轴:侧栏沿 y,底栏沿 x。唯一真相在 app.slint。
//!
//! 这块背景**只在切 tab 的转场期间重渲**:转场结束后 Slint 继续合成最后一帧纹理,
//! 沿用仓库既有的 render-active 省电取向(不主动重绘)。
//!
//! 这里唯一需要单测的核心逻辑,是那道省电门 —— 判断"这一帧要不要重渲导航纹理"。
//! 几何的逻辑像素×缩放系数换算沿用 glass rect 的做法在通知回调里内联,不在此重造被测函数。

use slint::ComponentHandle;

use crate::{MainWindow, Theme};

/// 传给导航 shader 的一帧控制量,**物理像素**。POD,不含 wgpu 类型 —— 由 apps/* 在 seam
/// 处拷成 `render3d::NavParams`(镜像分离,理由同 [`SceneControls`](crate::SceneControls):
/// ui 与 render3d 刻意互不依赖)。
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct NavGlassControls {
    /// 导航条纹理尺寸(= 目标纹理尺寸)。
    pub strip_w: f32,
    pub strip_h: f32,
    /// 三颗球中心在**移动轴**上的位置,相对条首:头(快)/尾(慢)/小水滴(最慢)。
    /// 三条各自时长的动画朝同一个目标走,天然拉开先后(§11 的追随系数,
    /// 这里用 Slint animate 的时长差实现,不逐帧积分)。
    pub lead: f32,
    pub lag: f32,
    pub drop: f32,
    /// 三颗球在**固定轴**上的中心。侧栏取栏宽的一半;底栏的图标被手势条 inset
    /// 顶上去了,取的是除掉那段之后的一半,不是纹理正中。
    pub cross: f32,
    /// 移动轴上一格的长度(侧栏是项高,底栏是等分出来的格宽),选中块的参考尺寸。
    pub slot: f32,
    /// 固定轴上的**可用厚度**。侧栏是栏宽;底栏是图标那一带的高,不含手势条
    /// inset —— 拿整张纹理的高当厚度,水滴会压到手势条上去。
    pub thick: f32,
    /// 移动轴是 x(手机底栏)还是 y(宽版式侧栏)。同一套数学,轴对调(#70)。
    pub horizontal: bool,
    /// 三球的整体缩放,1 常态、0 缩没。侧栏底部两颗自 #71 起是 glass 圆钮、
    /// 不在轨道上,选中它们时水滴化掉,免得上面还亮着一格。
    pub ball: f32,
    /// 深色主题。导航背景是自绘的,采不到背后的像素,主题只能这样传进去。
    pub dark: bool,
}

/// 省电门:这一帧要不要重渲导航选中器纹理。
///
/// `lead`/`lag` 是本帧读到的两个动画位置(选中块的头、尾中心,物理或逻辑像素皆可,
/// 只要与 `last` 同一坐标系)。`last` 是上一帧读到的 `(lead, lag)`,首帧为 `None`。
///
/// 规则:
/// - 首帧(`last` 为 `None`)必渲一次,建立静止态纹理(否则侧栏首屏没有底);
/// - 之后仅当 `(lead, lag)` 相对上一帧发生变化(有一个还在动 = 转场进行中)才渲;
/// - 两者都与上一帧相等(转场已结束、或本就没在切)则跳过,复用上一帧纹理。
///
/// 为何要这道门:3D 页每帧都强制 `request_redraw` → 渲染通知每帧都来,若不判定就会
/// 把静止不动的导航每帧白重渲一遍。稳定期用相等判定跳过 —— Slint 未在 animate 时会把
/// 属性值原样保持,逐帧精确相等,故直接相等比较即可,无需 epsilon。转场结束那一帧值仍
/// 与上帧不同 → 会重渲一次拿到最终静止态,再下一帧才相等跳过,不漏最后一帧。
pub fn nav_transition_active(
    lead: f32,
    lag: f32,
    drop: f32,
    last: Option<(f32, f32, f32)>,
) -> bool {
    match last {
        // 首帧:还没有静止态纹理,必渲一次。
        None => true,
        // 有一个位置相对上一帧变了 = 转场还在走(含刚点下时进度跳变);都没变则跳过。
        Some(prev) => (lead, lag, drop) != prev,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 首帧:last 为 None 时必须重渲,好让侧栏首屏就有静止态纹理。
    #[test]
    fn first_frame_forces_render() {
        assert!(nav_transition_active(
            10.0, 10.0, 10.0, None
        ));
    }

    /// 转场进行中:三球任一相对上一帧变化(还在走)时重渲。
    #[test]
    fn moving_position_triggers_render() {
        // 只有 lead 在动
        assert!(nav_transition_active(
            12.0,
            8.0,
            8.0,
            Some((10.0, 8.0, 8.0))
        ));
        // 只有 lag 在动
        assert!(nav_transition_active(
            12.0,
            9.0,
            8.0,
            Some((12.0, 8.0, 8.0))
        ));
        // 只有小水滴还在走 —— 它最慢,常常是最后一个到的。
        assert!(nav_transition_active(
            12.0,
            12.0,
            10.0,
            Some((12.0, 12.0, 9.0))
        ));
    }

    /// 稳定期:三球都与上一帧相等(转场结束/未切)时跳过,复用上一帧纹理。
    #[test]
    fn settled_positions_skip_render() {
        assert!(!nav_transition_active(
            10.0,
            10.0,
            10.0,
            Some((10.0, 10.0, 10.0))
        ));
    }

    /// 转场结束那一帧:三球刚齐齐抵达终点、仍不同于上帧 → 重渲一次拿到最终静止态。
    #[test]
    fn final_settling_frame_still_renders_once() {
        assert!(nav_transition_active(
            10.0,
            10.0,
            10.0,
            Some((11.0, 9.0, 8.0))
        ));
    }
}

/// 选中器的跨帧省电门:上一帧的三球逻辑位置、条的物理尺寸、明暗与球体缩放。
///
/// 这四样都进判据 —— 静止时 Slint 复用上一帧 `nav-bg`,漏掉哪一样,那一样变了
/// 画面就不跟,要等下一次切 tab 才补上。
#[derive(Default)]
pub struct NavSelector {
    last_ll: Option<(f32, f32, f32)>,
    last_size: Option<(f32, f32)>,
    last_dark: Option<bool>,
    last_ball: Option<f32>,
}

impl NavSelector {
    /// 这一帧要不要重渲选中器;要就渲一张推给界面。
    pub fn tick(
        &mut self,
        ui: &MainWindow,
        scale: f32,
        nav_frame: &mut impl FnMut(
            &NavGlassControls,
        )
            -> Option<slint::Image>,
    ) {
        // ── 导航液态玻璃选中器(宽版式侧栏 / 紧凑版式底栏)──
        // 常驻,与下面播放页视觉的门相互独立:只在切 tab 的 metaball 还在走
        // (三球位置相对上一帧变化)或条尺寸变化时重渲,静止时 Slint 复用上一帧 nav-bg。
        // 轴的事全在 .slint 那侧算完,这里只搬数(#70)。
        if ui.get_nav_visible() {
            let lead = ui.get_nav_lead();
            let lag = ui.get_nav_lag();
            let drop = ui.get_nav_drop();
            let strip_w =
                (ui.get_nav_strip_w() * scale).max(1.0);
            let strip_h =
                (ui.get_nav_strip_h() * scale).max(1.0);
            let size_changed =
                self.last_size != Some((strip_w, strip_h));
            // 主题也要进判据:侧栏背景是这条 pass 自绘的,而这道门静止时
            // 复用上一帧纹理 —— 不认主题的话,换了明暗侧栏仍是旧配色,
            // 要等下一次切 tab 才跟上。
            let dark = ui.global::<Theme>().get_dark();
            let theme_changed =
                self.last_dark != Some(dark);
            // 球体缩放同理:切到侧栏底部那两颗圆钮时槽位不动、只有它在变
            // (#71),不认它的话水滴就化不掉,停在原地不动。
            let ball = ui.get_nav_ball();
            let ball_changed = self.last_ball != Some(ball);
            if nav_transition_active(
                lead,
                lag,
                drop,
                self.last_ll,
            ) || size_changed
                || theme_changed
                || ball_changed
            {
                if let Some(img) =
                    (nav_frame)(&NavGlassControls {
                        strip_w,
                        strip_h,
                        lead: lead * scale,
                        lag: lag * scale,
                        drop: drop * scale,
                        cross: ui.get_nav_cross() * scale,
                        slot: ui.get_nav_slot() * scale,
                        thick: ui.get_nav_thick() * scale,
                        horizontal: ui.get_nav_horizontal(),
                        ball,
                        dark,
                    })
                {
                    ui.set_nav_bg(img);
                }
                self.last_ll = Some((lead, lag, drop));
                self.last_dark = Some(dark);
                self.last_ball = Some(ball);
                self.last_size = Some((strip_w, strip_h));
            }
        }
    }
}
