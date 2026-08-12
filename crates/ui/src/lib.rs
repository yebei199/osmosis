//! UI 层:界面的声明,以及界面与客户端领域([`app_core`])之间的双向绑定。
//!
//! 本 crate 也是**组装点**:它把 [`api`] 的请求函数注入 [`app_core`],
//! 让二者互不相识。依赖方向单向,反向永久禁止。见 `docs/adr/0003`。
//! 各平台入口(`apps/*`)在初始化好渲染后端之后调用 [`run`]。

slint::include_modules!();

mod nav_glass;
pub use media::{
    MediaCommand, MediaControls, MediaHooks, MediaStatus,
    NoControls, NowPlaying,
};
// 循环三态过 seam 原样透传:平台层(MPRIS/安卓)拿它翻成各自的方言。
pub use app_core::LoopMode;
pub use nav_glass::NavGlassControls;

mod viz;
pub use viz::{
    CoverUpdate, VIZ_AUDIO_BYTES, VizControls, VizCover,
    VizImages, VizPointer,
};

// 封面解码用到 image,是原生 target 的依赖(web 的封面等播放链路通了一起做)。
#[cfg(not(target_arch = "wasm32"))]
mod cover;

// 登录页的绑定。所有端都要 —— 音乐相关的路由一律要登录态。
mod account;
// 歌单列表与详情。与 music 分开:那边管的是「一批歌」,这边管的是「哪一批」。
// 与 artwork 同一道门:歌单封面要它,而它是原生 target 的依赖。用它的地方
// (music 的 Deck、search)本来就都在门里。
#[cfg(not(target_arch = "wasm32"))]
mod playlist;
// 歌单封面:取一次、记住、下次直接给。
#[cfg(not(target_arch = "wasm32"))]
mod artwork;
// 曲目行的缩略图。与 artwork 分开是因为键不同(封面 URL vs 歌单 id),
// 因而缓存、去重与淘汰的规则全都不同。
#[cfg(not(target_arch = "wasm32"))]
mod thumbnail;
// 红心:哪些歌在红心里,以及点一下之后发生什么。
mod liked;
// 播放进度的格式化。与列表里的时长同一条规矩:算在 Rust 侧,`.slint` 里只摆。
mod progress;
// 搜索的三个页签。歌曲那一路借 music 的队列,歌手与歌单各自成列。
#[cfg(not(target_arch = "wasm32"))]
mod search;

// 一次性提示的唯一出口。所有端都要 —— 报错的路各端都有。
mod notice;

mod media;
mod music;
// 明暗主题。颜色在 slint/theme.slint,这里只管那一位布尔值住在哪。
mod aurora;
mod aurora_btn;
pub use aurora_btn::{
    AuroraBtnControls, AuroraBtnSlotControls,
};
// 卡墙的几何与交互动力学,纯数学、无 GPU 可测(adr/0025)。
pub mod wall;
// 卡墙的每帧驱动与 slint 绑定,seam 类型也在这。
mod wall_drive;
pub use wall_drive::{
    WallCardControls, WallControls, WallCoverControls,
    WallDrive,
};
mod profile;
mod theme;
// 同播只在原生上有:wasm 没有 WebRTC 之外的音频栈可推(见 `Cargo.toml` 的条件依赖)。
#[cfg(not(target_arch = "wasm32"))]
mod syncplay;

use slint::{ComponentHandle, RenderingState};

/// 帧率读数开不开。`OSMOSIS_FPS` 设成任意值即开,与 `OSMOSIS_TAB` 同属调试开关。
///
/// 曾经是个 feature,但它不门控任何依赖 —— 关掉省下的只有一个 2Hz 定时器和每帧一次自增,
/// 却要在四个 manifest 里各声明一遍、还被三个 `bevy-3d` 隐含。开发时想不想看,本就不是
/// 编译期该管的事。
///
/// 两条路都要:桌面读运行期环境变量,拨开关不必重编;wasm 与 APK 读不到运行期环境变量
/// (页面由浏览器拉起、APK 由系统拉起),只能构建期烧进去 —— 同 `apps/android` 待
/// `SLINT_MCP_PORT` 的办法。
fn fps_enabled() -> bool {
    std::env::var("OSMOSIS_FPS").is_ok()
        || option_env!("OSMOSIS_FPS").is_some()
}

/// 最大页签下标:0=Home、1=Music。
///
/// 与 `app.slint` 里 `Nav.items` 的条数手工对齐 —— Slint 的全局属性不能当 Rust 常量用,
/// 加页时两处都要动。加漏了的症状是「`OSMOSIS_TAB=2` 静默停在 Music 页」。
const MAX_TAB: i32 = 3;

/// 创建窗口并完成所有领域状态绑定。[`run`] 与 [`run_with_renderers`] 的公共前半段。
///
/// 顺带交出可视化的数据源(频谱分析器句柄):它由 music 的播放器产出,
/// 而消费它的渲染通知回调装在 [`run_with_renderers`] 里 —— 两处只在这里相遇。
///
/// 调用前平台入口必须已经初始化好 slint 的渲染后端。
fn build_ui(
    media: impl FnOnce(MediaHooks) -> Box<dyn MediaControls>,
) -> (
    MainWindow,
    viz::Source,
    music::LyricFeed,
    music::CoverFeed,
) {
    let ui = MainWindow::new()
        .expect("failed to create main window");

    // 先恢复上次的登录态,再绑界面 —— 绑定那一步会按登录与否决定先拉什么。
    // 恢复出来的 token 可能已被服务端吊销,那要等第一次请求 401 才知道。
    api::session::restore();
    // 接登录页。它按恢复出来的会话决定开局是登录页还是主界面。
    account::bind(&ui);

    // 主题要在别的绑定之前恢复:颜色是全局的,晚一步会让开局那一帧
    // 用错配色闪一下。
    theme::bind(&ui);
    profile::bind(&ui);
    aurora_btn::bind(&ui);

    let (viz_source, lyrics, cover) =
        music::bind(&ui, media);

    ui.set_show_fps(fps_enabled());
    ui.set_platform(platform_name().into());
    // 设置页「关于」那一行。版本取本 crate 的(workspace 里同一个版本号)。
    ui.set_about_line(
        format!(
            "Osmosis {} · Slint + Bevy",
            env!("CARGO_PKG_VERSION")
        )
        .into(),
    );
    // 开局停在哪一页。默认 Home,`OSMOSIS_TAB` 覆盖它 —— 那是调试开关,
    // `just shot 420 1` 靠它直接截到 Music 页,不必再靠 MCP 模拟点击(那条路上有一串
    // 静默失败的坑,见 AGENTS.md)。没设或设歪了就留在 Home。
    if let Ok(tab) = std::env::var("OSMOSIS_TAB")
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
    // 这条路上的端(web / iOS)还没有系统媒体控件的实现。
    let (ui, _viz_source, _lyrics, _cover) =
        build_ui(|_| Box::new(NoControls));
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
/// 播放页展开的每一帧,以 [`VizControls`](播放页时钟 + 音频纹理字节)和窗口
/// **物理像素**尺寸调用,返回的三张图分别推到 `viz-bg` / `viz-scene` / `viz-occluder`。
/// 暂停与失焦照样调用(前台恒满帧);只有收起播放页才停。
///
/// 调用前平台入口必须已经用**共享的** wgpu device 配好 Slint 后端,否则闭包产出的纹理
/// 不属于 Slint 的 device,采样不出来。
///
/// `media` 交出这一端的系统媒体控件后端(见 [`MediaControls`] 与 `docs/adr/0020`)。
/// 它收到的 [`MediaHooks`] 要等窗口与播放器都造好才存在,所以是这里回头调它,
/// 而不是入口先造好塞进来。没有实现的端传 [`NoControls`]。
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
    mut btn_frame: impl FnMut(
        &AuroraBtnControls,
    ) -> Vec<slint::Image>
    + 'static,
    mut wall_frame: impl FnMut(
        &WallControls,
    ) -> Option<slint::Image>
    + 'static,
    media: impl FnOnce(MediaHooks) -> Box<dyn MediaControls>,
) {
    let (ui, viz_source, lyrics, cover) = build_ui(media);
    // 卡墙状态:回调(点击/滚轮)与渲染循环共享同一份。
    let wall_state = std::rc::Rc::new(
        std::cell::RefCell::new(
            wall_drive::WallDrive::new(),
        ),
    );
    wall_drive::bind(&ui, &wall_state);
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
    let mut nav_last_ll: Option<(f32, f32, f32)> = None;
    let mut nav_last_size: Option<(f32, f32)> = None;
    let mut nav_last_dark: Option<bool> = None;

    // 光带按钮的跨帧状态:振幅动画 + 按钮时钟,每帧推进。
    // bar 是 fluid 正在播放胶囊(#68):播放当"热",暂停收回静息。
    let mut btn_home = aurora_btn::ButtonAnim::default();
    let mut btn_daily = aurora_btn::ButtonAnim::default();
    let mut btn_bar = aurora_btn::ButtonAnim::default();
    let mut btn_time = 0.0f32;
    let mut btn_last: Option<web_time::Instant> = None;
    // 播放页时钟:播放页开着就累加(暂停也动);收起再展开时从定格处继续,
    // 不跳变。
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
                let drop = ui.get_nav_drop_y();
                let strip_w =
                    (ui.get_nav_w() * scale).max(1.0);
                let strip_h =
                    (ui.get_nav_h() * scale).max(1.0);
                let size_changed = nav_last_size
                    != Some((strip_w, strip_h));
                // 主题也要进判据:侧栏背景是这条 pass 自绘的,而这道门静止时
                // 复用上一帧纹理 —— 不认主题的话,换了明暗侧栏仍是旧配色,
                // 要等下一次切 tab 才跟上。
                let dark = ui.global::<Theme>().get_dark();
                let theme_changed =
                    nav_last_dark != Some(dark);
                if nav_glass::nav_transition_active(
                    lead,
                    lag,
                    drop,
                    nav_last_ll,
                ) || size_changed
                    || theme_changed
                {
                    if let Some(img) =
                        nav_frame(&NavGlassControls {
                            strip_w,
                            strip_h,
                            lead_y: lead * scale,
                            lag_y: lag * scale,
                            drop_y: drop * scale,
                            slot_h: ui.get_nav_slot_h()
                                * scale,
                            dark,
                        })
                    {
                        ui.set_nav_bg(img);
                    }
                    nav_last_ll = Some((lead, lag, drop));
                    nav_last_dark = Some(dark);
                    nav_last_size =
                        Some((strip_w, strip_h));
                }
            }

            // ── 光带按钮(§9)──
            // 两颗:Home 空槽(nebula)与空状态「换一批推荐」(ribbon 绿板)。
            // 前台恒满帧,每帧照渲;关掉开关即整段不进 —— 纯色实底,功能不变。
            if ui.get_aurora_buttons_on() {
                btn_home.step(
                    ui.get_home_slot_hover(),
                    (
                        ui.get_home_slot_px(),
                        ui.get_home_slot_py(),
                    ),
                );
                btn_daily.step(
                    ui.get_empty_daily_hover(),
                    (
                        ui.get_empty_daily_px(),
                        ui.get_empty_daily_py(),
                    ),
                );
                // 胶囊的 fluid:播放当"热"(振幅升到满),暂停收回静息。
                btn_bar
                    .step(ui.get_is_playing(), (0.72, 0.5));
                {
                    let now = web_time::Instant::now();
                    if let Some(last) = btn_last {
                        btn_time += now
                            .duration_since(last)
                            .as_secs_f32()
                            .min(0.1);
                    }
                    btn_last = Some(now);

                    // 绿色四色板:底/主/次/高光(handoff aurora-button.js 的 DEF)。
                    const GREENS: [[f32; 3]; 4] = [
                        [0.043, 0.075, 0.063],
                        [0.310, 0.478, 0.247],
                        [0.561, 0.769, 0.416],
                        [0.914, 0.969, 0.839],
                    ];
                    let compact = ui.get_compact();
                    let (hw, hh) = if compact {
                        (120.0, 150.0)
                    } else {
                        (168.0, 210.0)
                    };
                    // fluid 正在播放胶囊(#68):尺寸由 .slint 回写,
                    // 没歌或场区未量出时不渲这一槽。
                    let bar_w = ui.get_bar_w();
                    let bar_h = ui.get_bar_h();
                    let bar_on = ui.get_is_playing()
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
                                amp: btn_home.amp,
                                mode: 1.0,
                                bands: 3.0,
                                variant: aurora_btn::VARIANT_NEBULA,
                                progress: 0.0,
                                pointer: (
                                    btn_home.px,
                                    btn_home.py,
                                ),
                                colors: GREENS,
                            },
                            AuroraBtnSlotControls {
                                w: 150.0 * scale,
                                h: 38.0 * scale,
                                radius: 19.0 * scale,
                                seed: 8.1,
                                speed: 1.15,
                                amp: btn_daily.amp,
                                mode: 1.0, // 绿板:全光谱只准在 Home 空槽
                                bands: 3.0,
                                variant: aurora_btn::VARIANT_RIBBON,
                                progress: 0.0,
                                pointer: (
                                    btn_daily.px,
                                    btn_daily.py,
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
                            amp: btn_bar.amp,
                            mode: 1.0,
                            bands: 3.0,
                            variant: aurora_btn::VARIANT_FLUID,
                            progress: 0.0,
                            pointer: (btn_bar.px, btn_bar.py),
                            colors: GREENS,
                        });
                        slots.len() - 1
                    });
                    // 播放页覆层在场时的两槽(#69):主控条底与两颗次要圆钮
                    // 的 glass 底。覆层不在场就整个不进 —— 那时它们连元素
                    // 都还没实例化。
                    let viz_open = ui.get_play_page_open();
                    let viz_bar_w = ui.get_viz_bar_w();
                    let viz_bar_h = ui.get_viz_bar_h();
                    let viz_bar_i = (viz_open
                        && viz_bar_w > 1.0
                        && viz_bar_h > 1.0)
                        .then(|| {
                            let (variant, progress) =
                                aurora_btn::fluid_or_progress(
                                    ui.get_buffering(),
                                    ui.get_progress_ratio(),
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
                                // 不为同一条曲线养第二台收敛机。
                                amp: btn_bar.amp,
                                mode: 1.0,
                                bands: 3.0,
                                variant,
                                progress,
                                pointer: (0.72, 0.5),
                                colors: GREENS,
                            });
                            slots.len() - 1
                        });
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
                            variant: aurora_btn::VARIANT_GLASS,
                            progress: 0.0,
                            pointer: (0.5, 0.5),
                            colors: GREENS,
                        });
                        slots.len() - 1
                    });
                    let imgs = btn_frame(&AuroraBtnControls {
                        time: btn_time,
                        slots,
                    });
                    if let [home, daily, ..] = imgs.as_slice()
                    {
                        ui.set_home_slot_bg(home.clone());
                        ui.set_empty_daily_bg(daily.clone());
                    }
                    let at = |i: Option<usize>| {
                        i.and_then(|i| imgs.get(i)).cloned()
                    };
                    if let Some(img) = at(bar_i) {
                        ui.set_bar_fluid_bg(img);
                    }
                    if let Some(img) = at(viz_bar_i) {
                        ui.set_viz_bar_bg(img);
                    }
                    if let Some(img) = at(viz_glass_i) {
                        ui.set_viz_glass_bg(img);
                    }
                }
            } else {
                btn_last = None;
            }

            // ── 卡墙(#66) ──
            // 门:墙可见(wall-visible 已集齐分区/构建/曲目判据)∧ 播放页
            // 没开。前台恒满帧,静墙也每帧照渲;失焦不再是门
            // (可见即前台,见 change_log 2026-08-11 always-on-rendering)。
            if ui.get_wall_visible()
                && !ui.get_play_page_open()
            {
                if let Some(controls) =
                    wall_state.borrow_mut().frame(&ui)
                {
                    if let Some(img) =
                        wall_frame(&controls)
                    {
                        ui.set_wall_bg(img);
                    }
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
            // 门只剩一条:播放页展开。暂停照样动(没音频时点云按环境节奏慢转),
            // 失焦照样动 —— 前台恒满帧,可见即前台。
            if ui.get_play_page_open() {
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
                            // 换歌那一帧才有动作(清空/换图),取走即回到"没消息"。
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
                            // 现在一张深度卡片都没有:歌词改成与歌名同层,
                            // 画在粒子之上(见 docs/adr/0010 的「歌词是例外」)。
                            // 下一张深度卡片回来时把这里置真,那台相机就醒了。
                            needs_occluder: false,
                        },
                        size.width,
                        size.height,
                    ) {
                        ui.set_viz_bg(imgs.warp);
                        ui.set_viz_scene(imgs.scene);
                        ui.set_viz_occluder(imgs.occluder);
                    }
                }
            } else {
                viz_last = None;
            }

            // 前台恒满帧:每帧都要下一帧,循环自持在 vsync 节拍上
            // (change_log 2026-08-11 always-on-rendering,推翻硬规则 7)。
            // 后台自然暂停:安卓切后台、桌面最小化时平台不再派发重绘,
            // 这里的请求落空,回调停摆;回前台第一帧由平台重启。
            ui.window().request_redraw();

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
