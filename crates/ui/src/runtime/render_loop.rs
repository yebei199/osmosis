//! 帧驱动:挂在 Slint 的渲染通知上,每帧组装参数、驱动各渲染器、推结果回界面,
//! 再按各渲染器交回的需求决定要不要下一帧(按需渲染,#153,`docs/adr/0035`)。

// 循环三态过 seam 原样透传:平台层(MPRIS/安卓)拿它翻成各自的方言。
pub use crate::shader::nav_glass::NavGlassControls;

use std::cell::RefCell;
use std::rc::Rc;

use web_time::Instant;

use crate::Shell;
use crate::Viz;
use crate::runtime::frame_stats::{
    FrameAccounting, fps, stall,
};
use crate::runtime::pace::{self, Demand, Pace};
use crate::wall::drive::WallDrive;
use crate::*;

type NavFn =
    dyn FnMut(&NavGlassControls) -> Option<slint::Image>;
type VizFn =
    dyn FnMut(&VizControls, u32, u32) -> Option<VizImages>;
type BtnFn =
    dyn FnMut(&AuroraBtnControls) -> Vec<slint::Image>;
type WallFn =
    dyn FnMut(&WallControls) -> Option<slint::Image>;

/// 平台入口注进来的五个渲染闭包(各自的约定见 [`run_with_renderers`])。
pub(crate) struct Renderers {
    pub(crate) nav: Box<NavFn>,
    pub(crate) viz: Box<VizFn>,
    pub(crate) btn: Box<BtnFn>,
    pub(crate) wall: Box<WallFn>,
    pub(crate) prewarm: Box<dyn FnMut() -> bool>,
}

/// 播放页时钟:活跃期内累加;定格、收起再展开时从定格处继续,不跳变。
#[derive(Default)]
struct VizClock {
    time: f32,
    last: Option<Instant>,
    /// 上一帧标注卡的视口锚点。既是下一帧遮挡层的开关,也是"锚点消失了"的判据。
    anchor: Option<(f32, f32)>,
}

impl VizClock {
    fn advance(&mut self, now: Instant) {
        if let Some(last) = self.last {
            self.time +=
                now.duration_since(last).as_secs_f32();
        }
        self.last = Some(now);
    }
}

/// 一帧的全部跨帧状态。渲染通知每帧调一次 [`FrameLoop::frame`]。
pub(crate) struct FrameLoop {
    renderers: Renderers,
    pace: Rc<Pace>,
    nav: crate::shader::nav_glass::NavSelector,
    band: crate::shader::aurora_btn::ButtonBand,
    /// 卡墙状态:回调(点击/滚轮)与渲染循环共享同一份。
    wall: Rc<RefCell<WallDrive>>,
    prewarm: crate::wall::Prewarm,
    /// 预热按时间走,而定格时没有帧:由它隔一会儿叫醒一次,预热完就停。
    prewarm_tick: slint::Timer,
    viz: VizClock,
    lyric: super::lyric_push::LyricPush,
    viz_source: viz::Source,
    lyrics: music::LyricFeed,
    cover: music::CoverFeed,
    size: Option<slint::PhysicalSize>,
}

impl FrameLoop {
    pub(crate) fn new(
        ui: &MainWindow,
        renderers: Renderers,
        pace: Rc<Pace>,
        wall: Rc<RefCell<WallDrive>>,
        (viz_source, lyrics, cover): (
            viz::Source,
            music::LyricFeed,
            music::CoverFeed,
        ),
    ) -> Self {
        let prewarm_tick = slint::Timer::default();
        let weak = ui.as_weak();
        prewarm_tick.start(
            slint::TimerMode::Repeated,
            crate::wall::PREWARM_EVERY,
            move || {
                if let Some(ui) = weak.upgrade() {
                    pace::wake(&ui);
                }
            },
        );
        Self {
            renderers,
            pace,
            nav: Default::default(),
            band: Default::default(),
            wall,
            prewarm: Default::default(),
            prewarm_tick,
            viz: VizClock::default(),
            lyric: Default::default(),
            viz_source,
            lyrics,
            cover,
            size: None,
        }
    }

    /// 画一帧,交回要不要下一帧。
    ///
    /// 要下一帧的只有两种:有东西在过渡(`busy`:刚换了图、镜头还在滑、纹理还没
    /// 就绪、卡面没传完),或者活跃期内有常驻的环境动效。其余时候让 Slint 空闲 ——
    /// 属性变了它自己会重绘,不经属性就改了画面的到达走 `runtime::pace` 叫醒。
    pub(crate) fn frame(
        &mut self,
        ui: &MainWindow,
        now: Instant,
    ) -> bool {
        // 窗口换了尺寸(拖窗口、转屏):各张纹理都要按新尺寸重来,当作一次输入。
        let size = ui.window().size();
        if self
            .size
            .replace(size)
            .is_some_and(|last| last != size)
        {
            self.pace.touch(now);
        }
        let live = self.pace.live(now);
        let scale = ui.window().scale_factor();

        let mut demand = Demand {
            busy: self.nav.tick(
                ui,
                scale,
                &mut self.renderers.nav,
            ),
            ambient: false,
        };
        demand |= self.band.tick(
            ui,
            scale,
            live,
            &mut self.renderers.btn,
        );
        demand |= self.wall_frame(ui, now, live);
        self.lyric.tick_line(ui, &self.lyrics);
        self.lyric.tick_window(ui, &self.lyrics);
        demand |= self.viz_frame(ui, now, live);
        demand.next_frame(live)
    }

    /// 卡墙(#66)。
    ///
    /// 门:墙可见(wall-visible 已集齐分区/构建/曲目判据)∧ 播放页没开。
    /// 墙在动或有卡面要传时必渲;静止的墙只在活跃期内照渲,之后定格。
    fn wall_frame(
        &mut self,
        ui: &MainWindow,
        now: Instant,
        live: bool,
    ) -> Demand {
        let shell = ui.global::<Shell>();
        let seen = shell.get_wall_visible()
            && !shell.get_play_page_open();
        // 墙还没露过面时,隔一会儿在后台预热一次(#121):第一次进卡墙
        // 那一帧就不必在主线程上现编三十多条管线。
        if self.prewarm.due(now, seen)
            && (self.renderers.prewarm)()
        {
            self.prewarm.finish();
        }
        if self.prewarm.done() {
            self.prewarm_tick.stop();
        }
        if !seen {
            return Demand::default();
        }
        let mut drive = self.wall.borrow_mut();
        let Some(controls) = drive.frame(ui) else {
            return Demand::default();
        };
        // 纹理没到手之前也不许停帧:冻在空图上墙就永远是空的。
        let missing = shell.get_wall_bg().size().width == 0;
        if !(live || drive.busy() || missing) {
            return Demand {
                busy: false,
                ambient: true,
            };
        }
        if let Some(img) = (self.renderers.wall)(&controls)
        {
            shell.set_wall_bg(img);
        }
        Demand {
            busy: true,
            ambient: true,
        }
    }

    /// warp 与播放页场景。
    ///
    /// **两道门,不是一道**(#87):warp 那张 192×192 的小图只要手上有歌就在 ——
    /// 环形播放键里那圈画面靠它,而键在每一页上都有。场景那一趟(封面点云 +
    /// 标注卡 + 遮挡层)只在播放页开着时走:它是 9216 个立方体的全绘,而且与卡墙
    /// 互斥 —— render_viz_frame 进门就把墙的相机关掉。
    ///
    /// 按需渲染的规矩(#153):光环与点云都只在活跃期内动,定格时有封面在等、
    /// 或纹理还没有,照样渲一帧。
    fn viz_frame(
        &mut self,
        ui: &MainWindow,
        now: Instant,
        live: bool,
    ) -> Demand {
        let player = ui.global::<Player>();
        if !player.get_has_track() {
            self.viz.last = None;
            return Demand::default();
        }
        let Some(audio) = viz::payload(&self.viz_source)
        else {
            return Demand::default();
        };
        let wants_scene =
            ui.global::<Shell>().get_play_page_open();
        let vz = ui.global::<Viz>();
        let needs_warp =
            live || vz.get_viz_bg().size().width == 0;
        let needs_scene = wants_scene
            && (live
                || self.cover.pending()
                || vz.get_viz_scene().size().width == 0);
        let ambient = Demand {
            busy: false,
            ambient: true,
        };
        if !needs_warp && !needs_scene {
            // 定格期间的时间不补:恢复那一帧从定格处接着走。
            self.viz.last = None;
            return ambient;
        }
        if live {
            self.viz.advance(now);
        } else {
            self.viz.last = None;
        }
        let size = ui.window().size();
        let Some(imgs) = (self.renderers.viz)(
            &VizControls {
                time: self.viz.time,
                audio,
                // 换歌那一帧才有动作(清空/换图),取走即回到"没消息"。
                // **只在渲场景那一帧取走**:不渲场景时取走等于把待处理的换封面
                // 消息丢进垃圾桶,等播放页真开时点云还挂着上一首的封面
                // (CONTEXT.md「封面点云」那个 bug 的同一个坑)。
                cover: if needs_scene {
                    self.cover.take()
                } else {
                    Default::default()
                },
                pointer: VizPointer {
                    x: vz.get_viz_pointer_x(),
                    y: vz.get_viz_pointer_y(),
                    down: vz.get_viz_pointer_down(),
                    active: vz.get_viz_pointer_active(),
                },
                preset: vz.get_viz_preset(),
                needs_warp,
                needs_scene,
                // 深度卡片是标注卡,它在画面里才需要遮挡层。用**上一帧**的锚点
                // 开关:锚点是这一帧渲染的产物,而这个开关是它的输入。差一帧看
                // 不出来,换来的是卡片转出画面时那第二遍全场景绘制立刻停。
                needs_occluder: needs_scene
                    && self.viz.anchor.is_some(),
            },
            size.width,
            size.height,
        ) else {
            return ambient;
        };
        if needs_warp {
            vz.set_viz_bg(imgs.warp);
        }
        // 场景那几样只在渲了场景时写 —— 没渲时写空图是白白的属性变更通知。
        if needs_scene {
            vz.set_viz_scene(imgs.scene);
            vz.set_viz_occluder(imgs.occluder);
            self.viz.anchor = imgs.anchor;
            if let Some((x, y)) = imgs.anchor {
                vz.set_viz_anchor_x(x);
                vz.set_viz_anchor_y(y);
            }
            vz.set_viz_anchor_visible(
                imgs.anchor.is_some(),
            );
        }
        Demand {
            busy: true,
            ambient: ambient.ambient,
        }
    }
}

/// 同 [`run`],但额外驱动导航侧栏的液态玻璃选中器与播放页视觉。带 bevy 的端
/// (桌面 / android)走这里。
///
/// `nav_frame` 由平台入口提供(见 `render3d::NavGlassPass`):切 tab 的转场期间,以物理像素
/// 的 [`NavGlassControls`] 调用,内部用独立 wgpu pass 画出侧栏背景纹理,返回其 `slint::Image`。
/// 导航栏常驻,选中器只在 metaball 还在走时重渲,静止后 Slint 复用上一帧纹理
/// (省电门 [`crate::shader::nav_glass::nav_transition_active`],再叠一道尺寸变化判定兜住窗口缩放)。
/// 返回 `None` 则这一帧不更新 `nav-bg`。
///
/// `viz_frame` 驱动播放页视觉(见 `render3d::WarpPass` 与 `Scene::render_viz_frame`):
/// 以 [`VizControls`](播放页时钟 + 音频纹理字节)和窗口**物理像素**尺寸调用,
/// 返回的三张图分别推到 `viz-bg` / `viz-scene` / `viz-occluder`。`needs_warp` 与
/// `needs_scene` 为假的那一半不必渲。
///
/// `wall_prewarm` 在卡墙还没露过面时隔一会儿调一次(见 `render3d::Scene::prewarm_wall`
/// 与 [`crate::wall::Prewarm`]),返回 `true` 表示管线都编好了,之后不再调。
///
/// 调用前平台入口必须已经用**共享的** wgpu device 配好 Slint 后端,否则闭包产出的纹理
/// 不属于 Slint 的 device,采样不出来。
///
/// `media` 交出这一端的系统媒体控件后端(见 [`MediaControls`] 与 `docs/adr/0020`)。
/// 它收到的 [`MediaHooks`] 要等窗口与播放器都造好才存在,所以是这里回头调它,
/// 而不是入口先造好塞进来。没有实现的端传 [`NoControls`]。
pub fn run_with_renderers(
    nav_frame: impl FnMut(
        &NavGlassControls,
    ) -> Option<slint::Image>
    + 'static,
    viz_frame: impl FnMut(
        &VizControls,
        u32,
        u32,
    ) -> Option<VizImages>
    + 'static,
    btn_frame: impl FnMut(
        &AuroraBtnControls,
    ) -> Vec<slint::Image>
    + 'static,
    wall_frame: impl FnMut(
        &WallControls,
    ) -> Option<slint::Image>
    + 'static,
    wall_prewarm: impl FnMut() -> bool + 'static,
    media: impl FnOnce(MediaHooks) -> Box<dyn MediaControls>,
) {
    // 连的是哪个后端烘在编译期,日志第一屏写明:debug 连本机、release 连生产,
    // 装错包时这一行就能看出来,不必等点歌报错。桌面与安卓都走这里。
    log::info!("服务端: {}", api::base_url());
    let (ui, pace, viz_source, lyrics, cover, frames) =
        build_ui(media);
    let wall_state =
        Rc::new(RefCell::new(WallDrive::new()));
    crate::wall::drive::bind(&ui, &wall_state);
    // 关掉时不建定时器(理由同 [`run`])。整个 Option 搬进下面的通知回调,Timer 随回调
    // 活到事件循环结束。
    let fps = fps_enabled().then(|| fps::start(&ui));
    let _stall = crate::stall_enabled().then(stall::start);

    // 一帧的account:回调里(我们:组装参数 + 驱动渲染器)与回调外(Slint 重绘整个
    // 界面 + 呈现)各占多少 —— 只有这个比值能说明该往哪边使劲。
    let mut frame_acct = FrameAccounting::default();
    let mut frame_loop = FrameLoop::new(
        &ui,
        Renderers {
            nav: Box::new(nav_frame),
            viz: Box::new(viz_frame),
            btn: Box::new(btn_frame),
            wall: Box::new(wall_frame),
            prewarm: Box::new(wall_prewarm),
        },
        pace,
        wall_state,
        (viz_source, lyrics, cover),
    );

    let weak = ui.as_weak();
    // 帧驱动挂在**渲染通知**上,不是定时器:渲染通知由 Slint 真正的重绘周期派发,
    // 原生上是 vsync。要不要下一帧由 FrameLoop 按需决定(#153);不要的时候 Slint
    // 空闲,属性变化、输入与 `runtime::pace` 的叫醒会再把它拉起来。
    // 后台自然暂停:安卓切后台、桌面最小化时平台不再派发重绘。
    //
    // 回调与其捕获的闭包由窗口持有,活到事件循环结束。
    ui.window()
        .set_rendering_notifier(move |state, _| {
            // AfterRendering 落在 Slint 画完的那一刻,是「在画」与「空等」的分界。
            if matches!(
                state,
                RenderingState::AfterRendering
            ) {
                frame_acct.end_rendering();
                frames.drawn();
                return;
            }
            if !matches!(
                state,
                RenderingState::BeforeRendering
            ) {
                return;
            }
            if let Some((frames, _)) = &fps {
                frames.set(frames.get() + 1);
            }
            frame_acct.begin_frame();
            let Some(ui) = weak.upgrade() else { return };
            if frame_loop.frame(&ui, Instant::now()) {
                ui.window().request_redraw();
            }
            frame_acct.end_callback();
        })
        .expect("渲染后端必须支持渲染通知");

    ui.run().expect("event loop failed");
}

#[cfg(test)]
mod tests {
    use core::time::Duration;
    use std::cell::Cell;

    use similar_asserts::assert_eq;
    use slint::Model as _;

    use super::*;
    use crate::runtime::pace::AMBIENT_IDLE;
    use crate::viz::CoverUpdate;

    /// 1×1 的一张图:渲染器「渲出来了」的样子。
    fn drawn() -> slint::Image {
        slint::Image::from_rgba8(slint::SharedPixelBuffer::<
            slint::Rgba8Pixel,
        >::new(1, 1))
    }

    /// 各渲染器被调了几次、最后一份播放页控制量长什么样。
    #[derive(Default)]
    struct Calls {
        btn: Cell<usize>,
        wall: Cell<usize>,
        warp: Cell<usize>,
        scene: Cell<usize>,
        /// 最后一次交给场景的封面动作:0 无、1 清空、2 换图。
        cover: Cell<u8>,
        /// 渲染器交回空图(纹理还没就绪)的模式。
        blank: Cell<bool>,
    }

    fn renderers(calls: &Rc<Calls>) -> Renderers {
        let image = {
            let calls = calls.clone();
            move || {
                if calls.blank.get() {
                    slint::Image::default()
                } else {
                    drawn()
                }
            }
        };
        let (c_btn, c_wall, c_viz) =
            (calls.clone(), calls.clone(), calls.clone());
        let (i_wall, i_viz) =
            (image.clone(), image.clone());
        Renderers {
            nav: Box::new(|_| None),
            btn: Box::new(move |controls| {
                c_btn.btn.set(c_btn.btn.get() + 1);
                controls
                    .slots
                    .iter()
                    .map(|_| drawn())
                    .collect()
            }),
            wall: Box::new(move |_| {
                c_wall.wall.set(c_wall.wall.get() + 1);
                Some(i_wall())
            }),
            viz: Box::new(move |v, _, _| {
                if v.needs_warp {
                    c_viz.warp.set(c_viz.warp.get() + 1);
                }
                if v.needs_scene {
                    c_viz.scene.set(c_viz.scene.get() + 1);
                    c_viz.cover.set(match v.cover {
                        CoverUpdate::Unchanged => 0,
                        CoverUpdate::Clear => 1,
                        CoverUpdate::Show(_) => 2,
                    });
                }
                Some(VizImages {
                    warp: if v.needs_warp {
                        i_viz()
                    } else {
                        slint::Image::default()
                    },
                    scene: if v.needs_scene {
                        i_viz()
                    } else {
                        slint::Image::default()
                    },
                    occluder: slint::Image::default(),
                    anchor: None,
                })
            }),
            prewarm: Box::new(|| true),
        }
    }

    /// 登录、停在首页、光带开着的一扇无头窗口,外加它的帧驱动与节拍。
    fn setup()
    -> (MainWindow, FrameLoop, Rc<Pace>, Rc<Calls>) {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");
        ui.global::<Session>().set_logged_in(true);
        ui.global::<Shell>().set_aurora_buttons_on(true);
        let pace = pace::bind(&ui);
        let calls = Rc::new(Calls::default());
        let frame_loop = FrameLoop::new(
            &ui,
            renderers(&calls),
            pace.clone(),
            Rc::new(RefCell::new(WallDrive::new())),
            (
                Some(audio::spectrum::Analyzer::new()),
                music::LyricFeed::silent(),
                music::CoverFeed::default(),
            ),
        );
        (ui, frame_loop, pace, calls)
    }

    /// 空闲时限之后的某一刻。
    fn idle() -> Instant {
        Instant::now() + AMBIENT_IDLE * 2
    }

    /// 首页静止十秒以后:画完最后一帧就不再要下一帧,Slint 进入空闲 ——
    /// 这正是手机前台发烫的那一处(#153)。
    #[test]
    fn a_still_home_page_stops_asking_for_frames() {
        let (ui, mut frame_loop, _, calls) = setup();

        assert!(
            frame_loop.frame(&ui, Instant::now()),
            "刚开局是活跃期,星云要一直动"
        );
        let later = idle();
        frame_loop.frame(&ui, later);
        let rendered = calls.btn.get();

        assert!(
            !frame_loop.frame(&ui, later),
            "静止了还在要帧"
        );
        assert_eq!(
            calls.btn.get(),
            rendered,
            "静止了还在渲"
        );
    }

    /// 定格之后任何输入立即恢复满帧。
    #[test]
    fn user_input_resumes_frames() {
        let (ui, mut frame_loop, _, _) = setup();
        frame_loop.frame(&ui, idle());
        assert!(!frame_loop.frame(&ui, idle()));

        ui.global::<Shell>().invoke_user_input();

        assert!(
            frame_loop.frame(&ui, Instant::now()),
            "摸了一下,环境动效该回来"
        );
    }

    /// 播放中十秒没人碰:光环与流体胶囊定格,不再要帧;活跃期内它们一直在动。
    #[test]
    fn playing_ambient_motion_stops_after_the_idle_limit() {
        let (ui, mut frame_loop, _, calls) = setup();
        ui.global::<Player>().set_has_track(true);
        ui.global::<Player>().set_is_playing(true);

        frame_loop.frame(&ui, Instant::now());
        assert!(frame_loop.frame(&ui, Instant::now()));
        let warps = calls.warp.get();
        assert!(warps >= 2, "活跃期内光环每帧都该渲");

        frame_loop.frame(&ui, idle());
        let frozen = calls.warp.get();
        assert!(!frame_loop.frame(&ui, idle()));
        assert_eq!(
            calls.warp.get(),
            frozen,
            "定格了光环还在渲"
        );
    }

    /// 换歌时封面异步到达:定格中的循环被叫醒,并把新封面交给点云。
    #[test]
    fn a_cover_arriving_while_frozen_reaches_the_point_cloud()
     {
        let (ui, mut frame_loop, pace, calls) = setup();
        ui.global::<Player>().set_has_track(true);
        ui.global::<Shell>().set_play_page_open(true);
        frame_loop.frame(&ui, Instant::now());
        let later = idle();
        frame_loop.frame(&ui, later);
        assert!(!frame_loop.frame(&ui, later));
        let scenes = calls.scene.get();

        // 封面到了:它经 pace 放行一小段,循环在那一段里把封面交出去。
        frame_loop.cover.replace(
            &ui,
            std::sync::Arc::new(VizCover {
                width: 1,
                height: 1,
                rgba: vec![0; 4],
            }),
        );
        assert!(
            pace.live(Instant::now()),
            "封面到了没叫醒循环"
        );
        assert!(frame_loop.frame(&ui, Instant::now()));

        assert!(calls.scene.get() > scenes);
        assert_eq!(
            calls.cover.get(),
            2,
            "新封面没交到点云手上"
        );
    }

    /// 纹理还没就绪(渲染器交回空图)时不许停帧,哪怕已经定格 ——
    /// 冻在空图上,那块画面就永远是空的(首帧冻结那一类 bug)。
    #[test]
    fn a_texture_that_is_not_ready_keeps_frames_coming() {
        let (ui, mut frame_loop, _, calls) = setup();
        calls.blank.set(true);
        ui.global::<Player>().set_has_track(true);

        assert!(
            frame_loop.frame(&ui, idle()),
            "光环纹理还没到手,不该停帧"
        );

        calls.blank.set(false);
        frame_loop.frame(&ui, idle());
        assert!(
            !frame_loop.frame(&ui, idle()),
            "纹理到手之后该停下"
        );
    }

    /// 卡墙:墙静止且过了活跃期就不再渲;缩略图到货(经 pace 叫醒的那一帧)
    /// 把新卡面传上去,传完才停。
    #[test]
    fn a_still_wall_freezes_and_a_thumbnail_arrival_uploads_it()
     {
        let (ui, mut frame_loop, _, calls) = setup();
        crate::wall::drive::bind(
            &ui,
            &frame_loop.wall.clone(),
        );
        let shell = ui.global::<Shell>();
        shell.set_current_tab(1);
        shell.set_wall_field_w(400.0);
        shell.set_wall_field_h(300.0);
        let tracks = Rc::new(slint::VecModel::from(vec![
            TrackRow {
                id: "a".into(),
                cover_url: "https://cdn/a.jpg".into(),
                ..Default::default()
            },
        ]));
        ui.global::<Player>().set_tracks(
            slint::ModelRc::from(tracks.clone()),
        );

        shell.invoke_set_view_wall(true);

        // 列表塌成墙、空白卡面传完,墙就静止了。
        let later = idle();
        let settled =
            (0..500).any(|_| !frame_loop.frame(&ui, later));
        assert!(settled, "墙一直没静下来");
        let walls = calls.wall.get();
        assert!(!frame_loop.frame(&ui, later));
        assert_eq!(
            calls.wall.get(),
            walls,
            "静止的墙还在渲"
        );

        // 缩略图到货:行里有了图,这一帧要把它传上去。
        let mut row = tracks.row_data(0).expect("有第一行");
        row.cover = drawn();
        tracks.set_row_data(0, row);
        assert!(
            frame_loop.frame(&ui, later),
            "卡面传上去之后要一帧上屏"
        );
        assert_eq!(calls.wall.get(), walls + 1);
        assert!(!frame_loop.frame(&ui, later));
    }

    /// 预热完就停掉它的叫醒定时器:不然定格时它会每 100ms 把循环拽起来一次。
    #[test]
    fn the_prewarm_timer_stops_once_prewarm_is_done() {
        let (ui, mut frame_loop, _, _) = setup();
        assert!(frame_loop.prewarm_tick.running());

        let t0 = Instant::now();
        frame_loop.frame(&ui, t0);
        frame_loop.frame(&ui, t0 + Duration::from_secs(2));

        assert!(frame_loop.prewarm.done());
        assert!(!frame_loop.prewarm_tick.running());
    }
}
