//! UI 层:界面的声明,以及界面与客户端领域([`app_core`])之间的双向绑定。
//!
//! 本 crate 也是**组装点**:它把 [`api`] 的请求函数注入 [`app_core`],
//! 让二者互不相识。依赖方向单向,反向永久禁止。见 `docs/adr/0003`。
//! 各平台入口(`apps/*`)在初始化好渲染后端之后调用 [`run`]。

slint::include_modules!();

mod nav_glass;
pub use nav_glass::NavGlassControls;

mod viz;
pub use viz::{
    VIZ_AUDIO_BYTES, VizControls, VizCover, VizImages,
    VizPointer,
};

// 封面解码用到 image,是原生 target 的依赖(web 的封面等播放链路通了一起做)。
#[cfg(not(target_arch = "wasm32"))]
mod cover;

mod music;
// 同播只在原生上有:wasm 没有 WebRTC 之外的音频栈可推(见 `Cargo.toml` 的条件依赖)。
#[cfg(not(target_arch = "wasm32"))]
mod syncplay;

use slint::{ComponentHandle, RenderingState};

/// 帧率读数开不开。`SLINT_STUDY_FPS` 设成任意值即开,与 `SLINT_STUDY_TAB` 同属调试开关。
///
/// 曾经是个 feature,但它不门控任何依赖 —— 关掉省下的只有一个 2Hz 定时器和每帧一次自增,
/// 却要在四个 manifest 里各声明一遍、还被三个 `bevy-3d` 隐含。开发时想不想看,本就不是
/// 编译期该管的事。
///
/// 两条路都要:桌面读运行期环境变量,拨开关不必重编;wasm 与 APK 读不到运行期环境变量
/// (页面由浏览器拉起、APK 由系统拉起),只能构建期烧进去 —— 同 `apps/android` 待
/// `SLINT_MCP_PORT` 的办法。
fn fps_enabled() -> bool {
    std::env::var("SLINT_STUDY_FPS").is_ok()
        || option_env!("SLINT_STUDY_FPS").is_some()
}

/// 最大页签下标:0=Home、1=Music。
///
/// 与 `app.slint` 里 `Nav.items` 的条数手工对齐 —— Slint 的全局属性不能当 Rust 常量用,
/// 加页时两处都要动。加漏了的症状是「`SLINT_STUDY_TAB=2` 静默停在 Music 页」。
const MAX_TAB: i32 = 1;

/// 创建窗口并完成所有领域状态绑定。[`run`] 与 [`run_with_renderers`] 的公共前半段。
///
/// 顺带交出可视化的数据源(频谱分析器句柄):它由 music 的播放器产出,
/// 而消费它的渲染通知回调装在 [`run_with_renderers`] 里 —— 两处只在这里相遇。
///
/// 调用前平台入口必须已经初始化好 slint 的渲染后端。
fn build_ui() -> (
    MainWindow,
    viz::Source,
    music::LyricFeed,
    music::CoverFeed,
) {
    let ui = MainWindow::new()
        .expect("failed to create main window");

    let (viz_source, lyrics, cover) = music::bind(&ui);

    ui.set_show_fps(fps_enabled());
    ui.set_platform(platform_name().into());
    // 开局停在哪一页。默认 Home,`SLINT_STUDY_TAB` 覆盖它 —— 那是调试开关,
    // `just shot 420 1` 靠它直接截到 Music 页,不必再靠 MCP 模拟点击(那条路上有一串
    // 静默失败的坑,见 AGENTS.md)。没设或设歪了就留在 Home。
    if let Ok(tab) = std::env::var("SLINT_STUDY_TAB")
        && let Ok(tab) = tab.parse::<i32>()
    {
        ui.set_current_tab(tab.clamp(0, MAX_TAB));
    }
    (ui, viz_source, lyrics, cover)
}

/// 创建窗口、绑定领域状态,然后运行事件循环直到窗口关闭。
///
/// 各平台入口在初始化好渲染后端后调用。不带 bevy 的端(web / ios)走这里:
/// 播放页覆层退回没有粒子与 warp 的形态,`.slint` 里零平台判断(见 [`VizImages`])。
pub fn run() {
    let (ui, _viz_source, _lyrics, _cover) = build_ui();
    // Timer 必须活到事件循环结束,否则会被立即析构、不再触发。
    // 关掉时连建都不建 —— 空转的 2Hz 唤醒在移动端是白耗电。
    let _fps_timer = fps_enabled().then(|| {
        let (frames, timer) = fps::start(&ui);
        // 无 bevy 的路径上没人装渲染通知,帧计数在这里自己接。
        ui.window()
            .set_rendering_notifier(move |state, _| {
                if matches!(
                    state,
                    RenderingState::BeforeRendering
                ) {
                    frames.set(frames.get() + 1);
                }
            })
            .ok();
        timer
    });

    ui.run().expect("event loop failed");
}

/// 同 [`run`],但额外驱动导航侧栏的液态玻璃选中器与播放页视觉。带 bevy 的端
/// (桌面 / android)走这里。
///
/// `nav_frame` 由平台入口提供(见 `render3d::NavGlassPass`):切 tab 的转场期间,以物理像素
/// 的 [`NavGlassControls`] 调用,内部用独立 wgpu pass 画出侧栏背景纹理,返回其 `slint::Image`。
/// 导航栏常驻,选中器只在 metaball 还在走时重渲,静止后 Slint 复用上一帧纹理
/// (省电门 [`nav_glass::nav_transition_active`],再叠一道尺寸变化判定兜住窗口缩放)。
/// 返回 `None` 则这一帧不更新 `nav-bg`。
///
/// `viz_frame` 驱动播放页视觉(见 `render3d::WarpPass` 与 `Scene::render_viz_frame`):
/// 门(展开 ∧ 播放 ∧ 窗口可见)开着的每一帧,以 [`VizControls`](播放页时钟 + 音频
/// 纹理字节)和窗口**物理像素**尺寸调用,返回的三张图分别推到 `viz-bg` / `viz-scene` /
/// `viz-occluder`。门关上就不再调用:暂停定格在最后一帧,时钟一并冻结,收起/失焦零重绘。
///
/// 调用前平台入口必须已经用**共享的** wgpu device 配好 Slint 后端,否则闭包产出的纹理
/// 不属于 Slint 的 device,采样不出来。
pub fn run_with_renderers(
    mut nav_frame: impl FnMut(
        &NavGlassControls,
    ) -> Option<slint::Image>
    + 'static,
    mut viz_frame: impl FnMut(
        &VizControls,
        u32,
        u32,
    ) -> Option<VizImages>
    + 'static,
) {
    let (ui, viz_source, lyrics, cover) = build_ui();
    // 关掉时不建定时器(理由同 [`run`])。整个 Option 搬进下面的通知回调,Timer 随回调
    // 活到事件循环结束。
    let fps = fps_enabled().then(|| fps::start(&ui));

    // 一帧的account:回调里(我们:组装参数 + 驱动渲染器)与回调外(Slint 重绘整个
    // 界面 + 浏览器合成/呈现)各占多少。web 上帧率被砍半时,只有这个比值能说明该往
    // 哪边使劲 —— render3d 自己的计时只覆盖回调内那一段,回调外从没被量过。
    let mut frame_acct = FrameAccounting::default();

    let weak = ui.as_weak();
    // 导航选中器的跨帧状态:上一帧的 (lead, lag) 逻辑位置与 (栏宽, 栏高) 物理尺寸,
    // 供省电门判定这一帧是否需要重渲(转场进行中 或 尺寸变化)。
    let mut nav_last_ll: Option<(f32, f32)> = None;
    let mut nav_last_size: Option<(f32, f32)> = None;
    // 播放页时钟:只在门开着的帧间累加,门关即冻结 —— 重开门时画面与运动
    // 都从定格处继续,不跳变。
    let mut viz_time = 0.0f32;
    let mut viz_last: Option<web_time::Instant> = None;
    // 上一次推给界面的歌词 (代际, 行号)。只在换行/换歌时推 —— 每帧无脑 set
    // 会把属性标脏,暂停定格与失焦零重绘就都白设了。
    let mut lyric_shown: Option<(u64, usize)> = None;
    // 帧驱动挂在**渲染通知**上,不是定时器。理由是 wasm:浏览器主线程唯一,合成、
    // 派发输入、跑 wasm 全挤在上面,固定间隔的 setTimeout 与合成器各跑各的 —— 间隔调小
    // 会把主线程占死(1ms 实测 rAF 掉到 2~8 次/秒,整个界面卡住),调大又硬性设了帧率
    // 上限(16ms 压在 ~62fps,实测只有 40)。渲染通知由 Slint 真正的重绘周期派发:
    // wasm 上是 requestAnimationFrame(合成完才给下一次,天然不会饿死主线程,且按显示器
    // 刷新率走),原生上是 vsync。两端都不再有人为的帧率上限,也不再有定时器与合成的错拍。
    //
    // 回调里改 `viz-*` 属性会标脏,画面在**下一帧**生效,故恒差一帧 —— 一帧延迟在
    // 环境视觉上看不出来,不为此发明双缓冲。
    // 回调与其捕获的闭包由窗口持有,活到事件循环结束。
    ui.window()
        .set_rendering_notifier(move |state, _| {
            // AfterRendering 落在 Slint 画完的那一刻,是「在画」与「空等」的分界。
            if matches!(
                state,
                RenderingState::AfterRendering
            ) {
                frame_acct.end_rendering();
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
            let scale = ui.window().scale_factor();

            // ── 导航侧栏液态玻璃选中器 ──
            // 常驻侧栏,与下面播放页视觉的门相互独立:只在切 tab 的 metaball 还在走
            // (lead/lag 相对上一帧变化)或栏尺寸变化时重渲,静止时 Slint 复用上一帧 nav-bg。
            // 紧凑版式(手机底栏)没有这条侧栏,nav-visible 为假,整段跳过。
            if ui.get_nav_visible() {
                let lead = ui.get_nav_lead_y();
                let lag = ui.get_nav_lag_y();
                let strip_w =
                    (ui.get_nav_w() * scale).max(1.0);
                let strip_h =
                    (ui.get_nav_h() * scale).max(1.0);
                let size_changed = nav_last_size
                    != Some((strip_w, strip_h));
                if nav_glass::nav_transition_active(
                    lead,
                    lag,
                    nav_last_ll,
                ) || size_changed
                {
                    if let Some(img) =
                        nav_frame(&NavGlassControls {
                            strip_w,
                            strip_h,
                            lead_y: lead * scale,
                            lag_y: lag * scale,
                            slot_h: ui.get_nav_slot_h()
                                * scale,
                        })
                    {
                        ui.set_nav_bg(img);
                    }
                    nav_last_ll = Some((lead, lag));
                    nav_last_size =
                        Some((strip_w, strip_h));
                }
            }

            // ── 播放页歌词 ──
            // 只在覆层展开时跟随:收起时歌词不可见,读位置纯属白耗。
            // 暂停时位置不前进,行自然定格,与省电门天然一致。
            if ui.get_play_page_open() {
                match lyrics.current() {
                    Some((generation, index, text, tr))
                        if lyric_shown
                            != Some((
                                generation, index,
                            )) =>
                    {
                        ui.set_lyric_line(text.into());
                        ui.set_lyric_translation(tr.into());
                        lyric_shown =
                            Some((generation, index));
                    }
                    None if lyric_shown.is_some() => {
                        // 换歌后的前奏:清空,不留上一首的最后一行。
                        ui.set_lyric_line(
                            slint::SharedString::new(),
                        );
                        ui.set_lyric_translation(
                            slint::SharedString::new(),
                        );
                        lyric_shown = None;
                    }
                    _ => {}
                }
            }

            // ── 播放页 warp 视觉 ──
            // 门三条:展开 ∧ 播放(.slint 的 viz-active)∧ 窗口聚焦。任一不满足即
            // 完全停手,Slint 复用上一帧 viz-bg:暂停定格,收起/失焦零重绘。
            // is_active 是 fork 加的公开 getter(yebei199/slint 11642f2),
            // 读的是最近一次 WindowActiveChanged 报告的激活状态。
            if ui.get_viz_active()
                && ui.window().is_active()
            {
                if let Some(audio) =
                    viz::payload(&viz_source)
                {
                    let now = web_time::Instant::now();
                    if let Some(last) = viz_last {
                        viz_time += now
                            .duration_since(last)
                            .as_secs_f32();
                    }
                    viz_last = Some(now);
                    let size = ui.window().size();
                    if let Some(imgs) = viz_frame(
                        &VizControls {
                            time: viz_time,
                            audio,
                            // 换歌解出新封面的那一帧才有值,取走即清空。
                            // `CoverPixels` 就是 `VizCover`(见 viz.rs),
                            // 直接交出去,不逐字段再抄一遍兆级的像素。
                            cover: cover.take(),
                            pointer: VizPointer {
                                x: ui.get_viz_pointer_x(),
                                y: ui.get_viz_pointer_y(),
                                down: ui
                                    .get_viz_pointer_down(),
                                active: ui
                                    .get_viz_pointer_active(
                                    ),
                            },
                            preset: ui.get_viz_preset(),
                        },
                        size.width,
                        size.height,
                    ) {
                        ui.set_viz_bg(imgs.warp);
                        ui.set_viz_scene(imgs.scene);
                        ui.set_viz_occluder(imgs.occluder);
                    }
                    ui.window().request_redraw();
                }
            } else {
                viz_last = None;
            }

            frame_acct.end_callback();
        })
        .expect("渲染后端必须支持渲染通知");

    ui.run().expect("event loop failed");
}

/// 每帧耗时的记账窗口(帧)。约两秒一行,与 render3d 的采样窗口对齐,便于两边日志对读。
const FRAME_ACCT_WINDOW: u32 = 120;

/// 把一帧切成三段:我们的回调、Slint 的渲染、以及空等。
///
/// 时间轴:`BeforeRendering` → 回调(组装参数 + 驱动渲染器)→ Slint 画整个界面 →
/// `AfterRendering` → 空等下一次 vsync → 下一个 `BeforeRendering`。
///
/// 只量到「回调外」是不够的:那一段里「在画」和「干等」混在一起,而两者的优化方向
/// 相反 —— 前者要减工作量,后者说明我们没超预算、该去看浏览器的呈现策略。
/// `AfterRendering` 正好落在二者的分界上。
#[derive(Default)]
struct FrameAccounting {
    /// 本帧进入回调的时刻,兼作 `AfterRendering` 的计时基准。
    start: Option<web_time::Instant>,
    /// 上一帧进入回调的时刻。首帧为 `None`,不计周期。
    last_start: Option<web_time::Instant>,
    /// 窗口内累计:(回调内, 回调进入→画完, 整帧周期),毫秒。
    totals: (f64, f64, f64),
    frames: u32,
}

impl FrameAccounting {
    /// 记录本帧起点,顺带累加与上一帧的间隔(即整帧周期)。
    fn begin_frame(&mut self) {
        let now = web_time::Instant::now();
        if let Some(prev) = self.last_start {
            self.totals.2 +=
                (now - prev).as_secs_f64() * 1000.0;
            self.frames += 1;
        }
        self.last_start = Some(now);
        self.start = Some(now);
    }

    /// 回调返回时调用,累加回调自身的耗时。
    fn end_callback(&mut self) {
        if let Some(start) = self.start {
            self.totals.0 +=
                start.elapsed().as_secs_f64() * 1000.0;
        }
    }

    /// Slint 画完时调用(`AfterRendering`);满一个窗口就打一行均值并清零。
    fn end_rendering(&mut self) {
        let Some(start) = self.start else { return };
        self.totals.1 +=
            start.elapsed().as_secs_f64() * 1000.0;
        if self.frames < FRAME_ACCT_WINDOW {
            return;
        }
        let n = f64::from(self.frames);
        let (callback, drawn, period) = (
            self.totals.0 / n,
            self.totals.1 / n,
            self.totals.2 / n,
        );
        log::info!(
            "ui: 近 {} 帧 —— 整帧 {period:.2}ms({:.0}fps)= 回调 {callback:.2}ms + Slint 渲染 {:.2}ms + 空等 {:.2}ms",
            self.frames,
            1000.0 / period,
            drawn - callback,
            period - drawn,
        );
        self.totals = (0.0, 0.0, 0.0);
        self.frames = 0;
    }
}

/// 当前编译目标的平台名,显示在标题里。
///
/// wasm 上 `std::env::consts::OS` 是 `"unknown"`,所以它得单独一支;其余各端
/// consts::OS 已经给出 android / ios / linux / windows / macos。
/// 全小写,与 cargo target 名对齐 —— 你看到的就是编译时选的那个 target。
fn platform_name() -> &'static str {
    if cfg!(target_arch = "wasm32") {
        "wasm"
    } else {
        std::env::consts::OS
    }
}

/// 帧率计。恒编译,开关在运行期([`fps_enabled`])—— 关掉时调用方不建定时器,这里零成本。
///
/// 帧数由调用方在渲染通知回调里累加**真实发生的**帧,每个采样周期算出帧率推给 UI。
/// 计数器不在这里接进渲染通知:一个窗口只能装一个通知回调,而 3D 路径要拿它当帧驱动
/// (见 [`run_with_renderer`](crate::run_with_renderer))—— 两边都装的话后者会顶掉前者,
/// 读数静默归零。故本模块只出计数器和采样定时器,由谁装通知、在哪儿 `bump` 交给调用方。
///
/// 刻意不主动请求重绘 —— Slint 是惰性渲染,空闲时本就不重绘,读数会自动趴到
/// ~1(交互/动画时才飙高),这正是诚实的即时帧率,也不会白耗电。3D 页每帧自请求重绘,
/// 这里自然就读到满帧。
mod fps {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use slint::{ComponentHandle, Timer, TimerMode};

    use crate::MainWindow;

    /// 采样周期。帧率 = 本周期内累计的帧数 / 周期秒数。
    const SAMPLE_PERIOD: Duration =
        Duration::from_millis(500);

    /// 启动帧率计,返回(待调用方每帧累加的计数器, 采样定时器)。
    ///
    /// [`Timer`] 必须由调用方持有到事件循环结束,否则会被立即析构、不再触发。
    pub(crate) fn start(
        ui: &MainWindow,
    ) -> (Rc<Cell<u32>>, Timer) {
        let frames = Rc::new(Cell::new(0u32));

        let weak_fps = ui.as_weak();
        let timer = Timer::default();
        let frames_sample = frames.clone();
        timer.start(
            TimerMode::Repeated,
            SAMPLE_PERIOD,
            move || {
                let counted = frames_sample.replace(0);
                if let Some(ui) = weak_fps.upgrade() {
                    ui.set_fps(
                        counted as f32
                            / SAMPLE_PERIOD.as_secs_f32(),
                    );
                }
            },
        );
        (frames, timer)
    }
}
