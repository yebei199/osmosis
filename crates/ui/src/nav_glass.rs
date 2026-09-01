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
    use similar_asserts::assert_eq;

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

    // ── 省电门接到真窗口上之后的样子([`NavSelector::tick`])──
    //
    // 上面几条钉的是判据本身,下面几条钉的是**判据都问到了没有**:
    // 主题、球体缩放、条尺寸各是一个独立的入口,漏掉哪一个,那一样变了
    // 画面就不跟,要等下一次切 tab 才补上 —— 而那不会报错。

    use std::cell::RefCell;
    use std::time::Duration;

    use crate::{Session, Shell};

    /// 一张 1×1 的图。渲染器给回什么不重要,给没给回才是被测的东西。
    fn image() -> slint::Image {
        slint::Image::from_rgba8(
            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(
                1, 1,
            ),
        )
    }

    /// 登录之后才有导航。宽版式(侧栏),`nav-visible` 恒为真。
    fn nav_window() -> MainWindow {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");
        ui.global::<Session>().set_logged_in(true);
        ui.global::<Shell>().set_compact(false);
        ui.global::<Shell>().set_current_tab(1);
        // 三球是 animate 属性:先读一遍建立静止态,免得首帧读到的是半路的值。
        i_slint_backend_testing::mock_elapsed_time(
            Duration::from_millis(900),
        );
        ui
    }

    /// 记下渲染器被喊了几次,以及每次收到的那份控制量。
    #[derive(Default)]
    struct Renders(RefCell<Vec<NavGlassControls>>);

    impl Renders {
        fn count(&self) -> usize {
            self.0.borrow().len()
        }

        fn last(&self) -> NavGlassControls {
            *self
                .0
                .borrow()
                .last()
                .expect("还没渲过任何一帧")
        }

        /// 一个只记账、返回假图的渲染器。真渲染在 render3d 那侧,要 GPU。
        fn recorder(
            &self,
        ) -> impl FnMut(&NavGlassControls) -> Option<slint::Image>
        {
            move |controls| {
                self.0.borrow_mut().push(*controls);
                Some(image())
            }
        }
    }

    /// 导航不在场时一帧都不渲。
    ///
    /// 紧凑版式下底栏是条件页面,没实例化时条宽是 0 —— 那时按 0 宽去渲,
    /// 拿到的是一张空纹理,而它会盖掉上一帧的好图。
    #[test]
    fn a_nav_that_is_not_on_screen_is_never_rendered() {
        let ui = nav_window();
        ui.global::<Shell>().set_compact(true);
        assert!(
            !ui.get_nav_visible(),
            "底栏还没实例化,前提不成立"
        );

        let renders = Renders::default();
        NavSelector::default().tick(
            &ui,
            1.0,
            &mut renders.recorder(),
        );

        assert_eq!(renders.count(), 0);
        assert_eq!(
            ui.get_nav_bg().size().width,
            0,
            "没渲的那一帧不该往 nav-bg 里塞东西"
        );
    }

    /// 首帧渲一张、推上去;紧接着的静止帧一张都不再渲。
    ///
    /// 3D 页每帧都强制重绘,渲染通知因此每帧都来。不判定的话,静止不动的
    /// 导航会被每帧白重渲一遍 —— 画面看不出任何区别,电却在掉。
    #[test]
    fn the_first_frame_renders_and_a_settled_one_does_not()
    {
        let ui = nav_window();
        let renders = Renders::default();
        let mut selector = NavSelector::default();

        selector.tick(&ui, 1.0, &mut renders.recorder());
        assert_eq!(renders.count(), 1);
        assert!(
            ui.get_nav_bg().size().width > 0,
            "渲出来的那张该推给界面,否则侧栏首屏没有底"
        );

        selector.tick(&ui, 1.0, &mut renders.recorder());
        assert_eq!(
            renders.count(),
            1,
            "什么都没变,这一帧不该再渲一次"
        );
    }

    /// 换了明暗要重渲,哪怕三球一动没动。
    ///
    /// 侧栏背景是这条 pass 自绘的,采不到背后的像素。不认主题的话,拨完
    /// 深色开关侧栏仍是旧配色,要等下一次切 tab 才跟上。
    #[test]
    fn flipping_the_theme_alone_still_renders() {
        let ui = nav_window();
        let renders = Renders::default();
        let mut selector = NavSelector::default();

        selector.tick(&ui, 1.0, &mut renders.recorder());
        let was = renders.last().dark;

        ui.global::<crate::Theme>().set_dark(!was);
        selector.tick(&ui, 1.0, &mut renders.recorder());

        assert_eq!(renders.count(), 2);
        assert_eq!(
            renders.last().dark,
            !was,
            "新的明暗要一并喂进去,不然重渲出来还是旧配色"
        );
    }

    /// 水滴化掉那一下也要重渲,哪怕槽位没动。
    ///
    /// 切到侧栏底部两颗圆钮(#71)时三球停在原处,只有球体缩放在变。
    /// 不认它的话水滴就化不掉,停在最后一格主项上不动。
    #[test]
    fn melting_the_droplet_alone_still_renders() {
        let ui = nav_window();
        let renders = Renders::default();
        let mut selector = NavSelector::default();

        selector.tick(&ui, 1.0, &mut renders.recorder());
        let before = renders.last();
        assert_eq!(
            before.ball, 1.0,
            "停在 Music 时水滴是满的"
        );

        // 2 是个人主页 —— 圆钮不在水滴轨道上,槽位不动、只有它缩没。
        ui.global::<Shell>().set_current_tab(2);
        i_slint_backend_testing::mock_elapsed_time(
            Duration::from_millis(900),
        );
        selector.tick(&ui, 1.0, &mut renders.recorder());

        let after = renders.last();
        assert_eq!(renders.count(), 2);
        assert_eq!(after.ball, 0.0);
        assert_eq!(
            (after.lead, after.lag, after.drop),
            (before.lead, before.lag, before.drop),
            "三球本就没动 —— 这一帧是缩放触发的,不是位置"
        );
    }

    /// 只有缩放系数变了(窗口挪到另一块屏)也要重渲。
    ///
    /// 三球位置存的是逻辑像素,换屏时它们一个字都不变;不认条的物理尺寸的话,
    /// 侧栏会一直沿用旧 DPI 那张纹理,直到下一次切 tab。
    #[test]
    fn a_scale_change_alone_still_renders() {
        let ui = nav_window();
        let renders = Renders::default();
        let mut selector = NavSelector::default();

        selector.tick(&ui, 1.0, &mut renders.recorder());
        let single = renders.last();

        selector.tick(&ui, 2.0, &mut renders.recorder());
        let double = renders.last();

        assert_eq!(renders.count(), 2);
        assert_eq!(
            double.strip_w,
            single.strip_w * 2.0,
            "条宽要按缩放换成物理像素"
        );
        assert_eq!(double.lead, single.lead * 2.0);
    }

    /// 渲染器交白卷时 nav-bg 不动,但这一帧仍算问过了。
    ///
    /// 非 GPU 构建里那条 pass 根本不存在。若因为没拿到图就不记账,下一帧
    /// 判据仍是「首帧」,于是每一帧都去问一次一个永远给不出图的渲染器。
    #[test]
    fn a_renderer_with_nothing_to_give_leaves_the_texture_alone()
     {
        let ui = nav_window();
        let asked = RefCell::new(0_u32);
        let mut blank = |_: &NavGlassControls| {
            *asked.borrow_mut() += 1;
            None
        };
        let mut selector = NavSelector::default();

        selector.tick(&ui, 1.0, &mut blank);
        selector.tick(&ui, 1.0, &mut blank);

        assert_eq!(
            *asked.borrow(),
            1,
            "交白卷也算问过了,静止的下一帧不该再问一次"
        );
        assert_eq!(ui.get_nav_bg().size().width, 0);
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
