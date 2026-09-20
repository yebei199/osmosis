//! 合批:一帧里哪几槽要渲,以及渲回来的图各摆到哪。
//!
//! 与上一层的分法:那边是一颗按钮自己的数学与设置开关,这边是「这一帧
//! 有几槽」的编排。三个可选槽都是按需追加的,下标错位不会报错,只会把
//! 玻璃底贴到胶囊上 —— 所以编排连同它的测试单独成一份。

use slint::ComponentHandle;

use super::*;
use crate::MainWindow;
use crate::Player;
use crate::Shell;

/// 两颗光带按钮与胶囊的跨帧动画状态,外加它们共用的那只时钟。
#[derive(Default)]
pub struct ButtonBand {
    home: ButtonAnim,
    daily: ButtonAnim,
    bar: ButtonAnim,
    time: f32,
    last: Option<web_time::Instant>,
}

impl ButtonBand {
    /// 推进一帧的动画,并把渲染出来的几张图推给界面。
    pub fn tick(
        &mut self,
        ui: &MainWindow,
        scale: f32,
        btn_frame: &mut impl FnMut(
            &AuroraBtnControls,
        ) -> Vec<slint::Image>,
    ) {
        // ── 光带按钮(§9)──
        // 两颗:Home 空槽(nebula)与空状态「换一批推荐」(ribbon 绿板)。
        // 前台恒满帧,每帧照渲;关掉开关即整段不进 —— 纯色实底,功能不变。
        if ui.global::<Shell>().get_aurora_buttons_on() {
            self.home.step(
                ui.global::<Shell>().get_home_slot_hover(),
                (
                    ui.global::<Shell>().get_home_slot_px(),
                    ui.global::<Shell>().get_home_slot_py(),
                ),
            );
            self.daily.step(
                ui.global::<Shell>()
                    .get_empty_daily_hover(),
                (
                    ui.global::<Shell>()
                        .get_empty_daily_px(),
                    ui.global::<Shell>()
                        .get_empty_daily_py(),
                ),
            );
            // 胶囊的 fluid:播放当"热"(振幅升到满),暂停收回静息。
            self.bar.step(
                ui.global::<Player>().get_is_playing(),
                (0.72, 0.5),
            );
            {
                let now = web_time::Instant::now();
                if let Some(last) = self.last {
                    self.time += now
                        .duration_since(last)
                        .as_secs_f32()
                        .min(0.1);
                }
                self.last = Some(now);

                // 绿色四色板:底/主/次/高光(handoff aurora-button.js 的 DEF)。
                const GREENS: [[f32; 3]; 4] = [
                    [0.043, 0.075, 0.063],
                    [0.310, 0.478, 0.247],
                    [0.561, 0.769, 0.416],
                    [0.914, 0.969, 0.839],
                ];
                let compact =
                    ui.global::<Shell>().get_compact();
                let (hw, hh) = if compact {
                    (120.0, 150.0)
                } else {
                    (168.0, 210.0)
                };
                // fluid 正在播放胶囊(#68):尺寸由 .slint 回写,
                // 没歌或场区未量出时不渲这一槽。
                let bar_w =
                    ui.global::<Shell>().get_bar_w();
                let bar_h =
                    ui.global::<Shell>().get_bar_h();
                let bar_on =
                    ui.global::<Player>().get_is_playing()
                        && bar_w > 1.0
                        && bar_h > 1.0;
                let mut slots = vec![
                    // 尺寸与 app.slint 的空槽/空状态键一致,改那边要同步这里。
                    AuroraBtnSlotControls {
                        w: hw * scale,
                        h: hh * scale,
                        radius: 22.0 * scale,
                        seed: 3.7,
                        speed: 1.0,
                        amp: self.home.amp,
                        mode: 1.0,
                        bands: 3.0,
                        variant: VARIANT_NEBULA,
                        progress: 0.0,
                        pointer: (
                            self.home.px,
                            self.home.py,
                        ),
                        colors: GREENS,
                    },
                    AuroraBtnSlotControls {
                        w: 150.0 * scale,
                        h: 38.0 * scale,
                        radius: 19.0 * scale,
                        seed: 8.1,
                        speed: 1.15,
                        amp: self.daily.amp,
                        mode: 1.0, // 绿板:全光谱只准在 Home 空槽
                        bands: 3.0,
                        variant: VARIANT_RIBBON,
                        progress: 0.0,
                        pointer: (
                            self.daily.px,
                            self.daily.py,
                        ),
                        colors: GREENS,
                    },
                ];
                // 后面几槽按需追加,记下各自的下标 —— 三个可选槽再用
                // 长度 match 就是八条臂,而错位不会报错,只会把玻璃底
                // 贴到胶囊上。
                let bar_i = bar_on.then(|| {
                    slots.push(AuroraBtnSlotControls {
                        w: bar_w * scale,
                        h: bar_h * scale,
                        // 宽版胶囊圆角 = 高的一半;紧凑版是 16px 圆角矩形。
                        radius: if compact {
                            16.0 * scale
                        } else {
                            bar_h * 0.5 * scale
                        },
                        seed: 5.3,
                        speed: 0.9,
                        amp: self.bar.amp,
                        mode: 1.0,
                        bands: 3.0,
                        variant: VARIANT_FLUID,
                        progress: 0.0,
                        pointer: (self.bar.px, self.bar.py),
                        colors: GREENS,
                    });
                    slots.len() - 1
                });
                // 播放页覆层在场时的两槽(#69):主控条底与两颗次要圆钮
                // 的 glass 底。覆层不在场就整个不进 —— 那时它们连元素
                // 都还没实例化。
                let viz_open = ui
                    .global::<Shell>()
                    .get_play_page_open();
                let viz_bar_w =
                    ui.global::<Shell>().get_viz_bar_w();
                let viz_bar_h =
                    ui.global::<Shell>().get_viz_bar_h();
                let viz_bar_i = (viz_open
                    && viz_bar_w > 1.0
                    && viz_bar_h > 1.0)
                    .then(|| {
                        let (variant, progress) =
                            fluid_or_progress(
                                ui.global::<Player>()
                                    .get_buffering(),
                                ui.global::<Player>()
                                    .get_progress_ratio(),
                            );
                        slots.push(AuroraBtnSlotControls {
                            w: viz_bar_w * scale,
                            h: viz_bar_h * scale,
                            // 与 app.slint 的 border-radius 同式。
                            radius: if compact {
                                16.0 * scale
                            } else {
                                viz_bar_h * 0.5 * scale
                            },
                            seed: 2.9,
                            speed: 0.85,
                            // 与胶囊同一个信号(播放当热),共用那份振幅,
                            // 不为同一条曲线养第二台收敛机。压掉四成:
                            // 控制键就压在这层上,紧凑版式的条又短,满幅的
                            // 羽流会把随机键与循环键的图标冲得读不出来
                            // (真机竖屏才看得出,桌面那条长而扁,亮核落在
                            // 时间读数那边)。
                            amp: self.bar.amp * 0.6,
                            mode: 1.0,
                            bands: 3.0,
                            variant,
                            progress,
                            pointer: (0.72, 0.5),
                            colors: GREENS,
                        });
                        slots.len() - 1
                    });
                // 侧栏底部两颗 glass 圆钮(#71)。各渲各的:选中那颗把振幅
                // 拉满,共用一张图就分不出谁被选中。侧栏只在宽版式存在。
                let rail_keys = !compact;
                let tab =
                    ui.global::<Shell>().get_current_tab();
                let mut rail_key = |i: i32, seed: f32| {
                    rail_keys.then(|| {
                        slots.push(nav_key_slot(
                            scale,
                            seed,
                            tab == i,
                            GREENS,
                        ));
                        slots.len() - 1
                    })
                };
                let key_a_i = rail_key(2, 4.6);
                let key_b_i = rail_key(3, 7.2);
                let viz_glass_i = viz_open.then(|| {
                    slots.push(AuroraBtnSlotControls {
                        // 与 widgets.slint 的 RoundControl 默认直径一致。
                        w: 44.0 * scale,
                        h: 44.0 * scale,
                        radius: 22.0 * scale,
                        seed: 6.4,
                        speed: 0.7,
                        // 两颗共用一张图,拆不出各自的悬停,底幅因此固定;
                        // glass 本就是低密度底,不靠振幅出戏。
                        amp: 0.55,
                        mode: 1.0,
                        bands: 3.0,
                        variant: VARIANT_GLASS,
                        progress: 0.0,
                        pointer: (0.5, 0.5),
                        colors: GREENS,
                    });
                    slots.len() - 1
                });
                let imgs =
                    (btn_frame)(&AuroraBtnControls {
                        time: self.time,
                        slots,
                    });
                if let [home, daily, ..] = imgs.as_slice() {
                    ui.global::<Shell>()
                        .set_home_slot_bg(home.clone());
                    ui.global::<Shell>()
                        .set_empty_daily_bg(daily.clone());
                }
                let at = |i: Option<usize>| {
                    i.and_then(|i| imgs.get(i)).cloned()
                };
                if let Some(img) = at(bar_i) {
                    ui.global::<Shell>()
                        .set_bar_fluid_bg(img);
                }
                if let Some(img) = at(viz_bar_i) {
                    ui.global::<Shell>()
                        .set_viz_bar_bg(img);
                }
                if let Some(img) = at(viz_glass_i) {
                    ui.global::<Shell>()
                        .set_viz_glass_bg(img);
                }
                if let Some(img) = at(key_a_i) {
                    ui.global::<Shell>()
                        .set_nav_key_a_bg(img);
                }
                if let Some(img) = at(key_b_i) {
                    ui.global::<Shell>()
                        .set_nav_key_b_bg(img);
                }
            }
        } else {
            self.last = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    // ── 合批([`ButtonBand::tick`])──
    //
    // 这一帧有几槽、各是哪一槽、渲回来的图分别摆到哪 —— 三个可选槽都是按需
    // 追加的,下标错位不会报错,只会把玻璃底贴到胶囊上(见 tick 里那段注释)。
    // 视觉在 render3d 那侧、要 GPU,所以渲染器是注进来的:这里只钉编排。

    use std::cell::RefCell;

    use crate::{MainWindow, Session};

    /// 编号即图宽:第 i 槽渲回来的图宽是 i+1 像素。
    ///
    /// 摆错位是这段代码唯一会犯的错,而它不报错 —— 只有让图自报家门才认得出。
    fn tagged(slot: usize) -> slint::Image {
        slint::Image::from_rgba8(
            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(
                slot as u32 + 1,
                1,
            ),
        )
    }

    /// 某张图是第几槽渲出来的。没图(宽 0)就是没摆上。
    fn slot_of(image: &slint::Image) -> Option<usize> {
        match image.size().width {
            0 => None,
            width => Some(width as usize - 1),
        }
    }

    /// 登录、光带开着、宽版式、没在放歌、播放页没开 —— 最素的一帧。
    fn band_window() -> MainWindow {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");
        ui.global::<Session>().set_logged_in(true);
        ui.global::<Shell>().set_aurora_buttons_on(true);
        ui.global::<Shell>().set_compact(false);
        ui.global::<Shell>().set_current_tab(0);
        ui
    }

    /// 记下每一帧交上来的控制量。
    #[derive(Default)]
    struct Frames(RefCell<Vec<AuroraBtnControls>>);

    impl Frames {
        fn count(&self) -> usize {
            self.0.borrow().len()
        }

        fn last(&self) -> AuroraBtnControls {
            self.0
                .borrow()
                .last()
                .expect("还没渲过任何一帧")
                .clone()
        }

        fn renderer(
            &self,
        ) -> impl FnMut(&AuroraBtnControls) -> Vec<slint::Image>
        {
            move |controls| {
                self.0.borrow_mut().push(controls.clone());
                (0..controls.slots.len())
                    .map(tagged)
                    .collect()
            }
        }
    }

    /// 关掉开关就整段不进:一槽不渲,已经摆上的图也不去动它。
    ///
    /// 关掉之后按钮退回纯色实底,功能不变 —— 但若仍照渲不误,省下的那点
    /// 开销正是这个开关存在的全部理由。
    #[test]
    fn switching_the_aurora_buttons_off_renders_nothing() {
        let ui = band_window();
        ui.global::<Shell>().set_aurora_buttons_on(false);
        let frames = Frames::default();

        ButtonBand::default().tick(
            &ui,
            1.0,
            &mut frames.renderer(),
        );

        assert_eq!(frames.count(), 0);
        assert_eq!(
            slot_of(
                &ui.global::<Shell>().get_home_slot_bg()
            ),
            None
        );
    }

    /// 最素的一帧:两颗光带按钮加侧栏底部那两颗圆钮,图各就各位。
    ///
    /// 胶囊与播放页那几槽都是按需追加的,没歌、没开播放页时它们不该在场 ——
    /// 多渲一槽就是多算一遍 fbm,而那张图没有任何地方会用到。
    #[test]
    fn an_idle_wide_frame_carries_the_two_ribbons_and_the_rail_keys()
     {
        let ui = band_window();
        let frames = Frames::default();

        ButtonBand::default().tick(
            &ui,
            1.0,
            &mut frames.renderer(),
        );

        let shell = ui.global::<Shell>();
        assert_eq!(
            frames.last().slots.len(),
            4,
            "空槽、绿板、侧栏两颗圆钮,不多不少"
        );
        assert_eq!(
            slot_of(&shell.get_home_slot_bg()),
            Some(0)
        );
        assert_eq!(
            slot_of(&shell.get_empty_daily_bg()),
            Some(1)
        );
        assert_eq!(
            slot_of(&shell.get_nav_key_a_bg()),
            Some(2)
        );
        assert_eq!(
            slot_of(&shell.get_nav_key_b_bg()),
            Some(3)
        );
        assert_eq!(
            slot_of(&shell.get_bar_fluid_bg()),
            None,
            "没在放歌,胶囊那一槽不该在场"
        );
    }

    /// 胶囊要等**既在放歌、又量出了尺寸**才进合批。
    ///
    /// 尺寸由 `.slint` 回写,场区没摆出来之前是 0 —— 拿 0 去渲得到一张空图,
    /// 而它会盖掉胶囊原本的底。
    #[test]
    fn the_capsule_waits_for_both_a_song_and_a_measured_size()
     {
        let ui = band_window();
        let frames = Frames::default();
        let mut band = ButtonBand::default();
        ui.global::<Player>().set_is_playing(true);

        band.tick(&ui, 1.0, &mut frames.renderer());
        assert_eq!(
            frames.last().slots.len(),
            4,
            "尺寸还没量出来,胶囊不该进合批"
        );

        ui.global::<Shell>().set_bar_w(240.0);
        ui.global::<Shell>().set_bar_h(60.0);
        band.tick(&ui, 1.0, &mut frames.renderer());

        let slots = frames.last().slots;
        assert_eq!(slots.len(), 5);
        assert_eq!(slots[2].variant, VARIANT_FLUID);
        assert_eq!(
            slot_of(
                &ui.global::<Shell>().get_bar_fluid_bg()
            ),
            Some(2),
            "胶囊要拿第 2 槽那张图,拿错就是把绿板贴到胶囊上"
        );
    }

    /// 播放页开着时多出两槽,而且两张图不能对调。
    ///
    /// 主控条底是长条、次要圆钮是 44×44 的玻璃底,下标错一位不会报错,
    /// 只会把玻璃底拉长贴到条上 —— 这正是 tick 里改用「记下标」的原因。
    #[test]
    fn the_play_page_overlay_adds_a_bar_and_a_glass_slot() {
        let ui = band_window();
        let shell = ui.global::<Shell>();
        shell.set_play_page_open(true);
        shell.set_viz_bar_w(300.0);
        shell.set_viz_bar_h(56.0);
        let frames = Frames::default();

        ButtonBand::default().tick(
            &ui,
            1.0,
            &mut frames.renderer(),
        );

        let slots = frames.last().slots;
        assert_eq!(
            slots.len(),
            6,
            "空槽、绿板、条底、两颗圆钮、玻璃底"
        );
        assert_eq!(slots[2].variant, VARIANT_FLUID);
        assert_eq!(slots[5].variant, VARIANT_GLASS);
        assert_eq!(
            slot_of(&shell.get_viz_bar_bg()),
            Some(2)
        );
        assert_eq!(
            slot_of(&shell.get_viz_glass_bg()),
            Some(5)
        );
    }

    /// 缓冲时播放页条底换成 progress 变体,并把当前进度一路喂到那一槽。
    ///
    /// 播放页的进度是播放键那个环,条上没有常驻细线 —— 缓冲时那道呼吸亮边
    /// 就是「还在动」的唯一信号(#69)。
    #[test]
    fn a_buffering_play_page_bar_shows_the_progress_variant()
     {
        let ui = band_window();
        let shell = ui.global::<Shell>();
        shell.set_play_page_open(true);
        shell.set_viz_bar_w(300.0);
        shell.set_viz_bar_h(56.0);
        ui.global::<Player>().set_buffering(true);
        ui.global::<Player>().set_progress_ratio(0.4);
        let frames = Frames::default();

        ButtonBand::default().tick(
            &ui,
            1.0,
            &mut frames.renderer(),
        );

        let bar = frames.last().slots[2];
        assert_eq!(bar.variant, VARIANT_PROGRESS);
        assert_eq!(bar.progress, 0.4);
    }

    /// 紧凑版式没有侧栏,那两颗圆钮整个不在场。
    ///
    /// 它们的元素只长在宽版式里。照渲的话是两张没人取的图,而下标还会
    /// 往后顶一位,把玻璃底挪到别人的位置上。
    #[test]
    fn the_compact_layout_drops_the_rail_keys() {
        let ui = band_window();
        ui.global::<Shell>().set_compact(true);
        let frames = Frames::default();

        ButtonBand::default().tick(
            &ui,
            1.0,
            &mut frames.renderer(),
        );

        assert_eq!(frames.last().slots.len(), 2);
        assert_eq!(
            slot_of(
                &ui.global::<Shell>().get_nav_key_a_bg()
            ),
            None
        );
    }

    /// 选中的那颗圆钮亮、另一颗暗。
    ///
    /// 水滴的轨道不覆盖这两格(#71),亮度是它们表达「我被选中」的全部手段 ——
    /// 两颗共用一份振幅的话,界面上就完全看不出停在哪一页。
    #[test]
    fn the_selected_rail_key_is_the_bright_one() {
        let ui = band_window();
        // 2 是个人主页,3 是设置 —— 侧栏底部那两颗。
        ui.global::<Shell>().set_current_tab(2);
        let frames = Frames::default();

        ButtonBand::default().tick(
            &ui,
            1.0,
            &mut frames.renderer(),
        );

        let slots = frames.last().slots;
        assert_eq!(slots[2].amp, 1.0, "选中那颗该拉满");
        assert!(
            slots[3].amp < slots[2].amp,
            "另一颗该停在低位,实得 {} 对 {}",
            slots[3].amp,
            slots[2].amp
        );
    }

    /// 按钮时钟从零起走,单帧最多补 0.1 秒,而关掉的那段一秒都不补。
    ///
    /// 时钟直接喂进着色器的相位。首帧若把进程启动到现在那一大段算进去,
    /// 第一眼看到的就是流场中途;关掉再打开时补上整段,则是肉眼可见的一跳。
    #[test]
    fn the_button_clock_starts_at_zero_and_never_jumps() {
        let ui = band_window();
        let frames = Frames::default();
        let mut band = ButtonBand::default();

        band.tick(&ui, 1.0, &mut frames.renderer());
        assert_eq!(
            frames.last().time,
            0.0,
            "首帧的相位该是 0"
        );

        std::thread::sleep(
            core::time::Duration::from_millis(150),
        );
        band.tick(&ui, 1.0, &mut frames.renderer());
        assert_eq!(
            frames.last().time,
            0.1,
            "掉了一帧最多补 0.1 秒"
        );

        // 关掉一段时间再打开:那段不该在重开那一帧一次性补进来。
        std::thread::sleep(
            core::time::Duration::from_millis(150),
        );
        ui.global::<Shell>().set_aurora_buttons_on(false);
        band.tick(&ui, 1.0, &mut frames.renderer());
        ui.global::<Shell>().set_aurora_buttons_on(true);
        band.tick(&ui, 1.0, &mut frames.renderer());

        assert_eq!(
            frames.last().time,
            0.1,
            "关掉期间的时间不该在重开那一帧补上"
        );
    }
}
