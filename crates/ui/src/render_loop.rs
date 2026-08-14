//! 帧驱动:挂在 Slint 的渲染通知上,每帧组装参数、驱动各渲染器、推结果回界面。

// 循环三态过 seam 原样透传:平台层(MPRIS/安卓)拿它翻成各自的方言。
pub use app_core::LoopMode;
pub use nav_glass::NavGlassControls;

use crate::Viz;
use crate::frame_stats::{FrameAccounting, fps};
use crate::*;
use slint::ComponentHandle as _;

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
    let wall_state =
        std::rc::Rc::new(std::cell::RefCell::new(
            wall_drive::WallDrive::new(),
        ));
    wall_drive::bind(&ui, &wall_state);
    // 关掉时不建定时器(理由同 [`run`])。整个 Option 搬进下面的通知回调,Timer 随回调
    // 活到事件循环结束。
    let fps = fps_enabled().then(|| fps::start(&ui));

    // 一帧的account:回调里(我们:组装参数 + 驱动渲染器)与回调外(Slint 重绘整个
    // 界面 + 浏览器合成/呈现)各占多少。web 上帧率被砍半时,只有这个比值能说明该往
    // 哪边使劲 —— render3d 自己的计时只覆盖回调内那一段,回调外从没被量过。
    let mut frame_acct = FrameAccounting::default();

    let weak = ui.as_weak();
    let mut nav = nav_glass::NavSelector::default();

    let mut band = aurora_btn::ButtonBand::default();
    // 播放页时钟:播放页开着就累加(暂停也动);收起再展开时从定格处继续,
    // 不跳变。
    let mut viz_time = 0.0f32;
    let mut viz_last: Option<web_time::Instant> = None;
    // 上一帧标注卡的视口锚点。既是下一帧遮挡层的开关,也是"锚点消失了"的判据。
    let mut viz_anchor: Option<(f32, f32)> = None;
    let mut lyric = lyric_push::LyricPush::default();
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

            nav.tick(&ui, scale, &mut nav_frame);
            band.tick(&ui, scale, &mut btn_frame);
            // ── 卡墙(#66) ──
            // 门:墙可见(wall-visible 已集齐分区/构建/曲目判据)∧ 播放页
            // 没开。前台恒满帧,静墙也每帧照渲;失焦不再是门
            // (可见即前台,见 change_log 2026-08-11 always-on-rendering)。
            if ui.get_wall_visible()
                && !ui.get_play_page_open()
                && let Some(controls) =
                    wall_state.borrow_mut().frame(&ui)
                && let Some(img) = wall_frame(&controls)
            {
                ui.set_wall_bg(img);
            }

            lyric.tick_line(&ui, &lyrics);

            lyric.tick_window(&ui, &lyrics);
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
                                x: ui
                                    .global::<Viz>()
                                    .get_viz_pointer_x(),
                                y: ui
                                    .global::<Viz>()
                                    .get_viz_pointer_y(),
                                down: ui
                                    .global::<Viz>()
                                    .get_viz_pointer_down(),
                                active: ui
                                    .global::<Viz>()
                                    .get_viz_pointer_active(
                                    ),
                            },
                            preset: ui
                                .global::<Viz>()
                                .get_viz_preset(),
                            // 深度卡片是标注卡,它在画面里才需要遮挡层。
                            // 用**上一帧**的锚点开关:锚点是这一帧渲染的产物,
                            // 而这个开关是它的输入,拿不到同帧的答案。差一帧看不出来,
                            // 换来的是卡片转出画面时那第二遍全场景绘制立刻停 ——
                            // 9216 个立方体再来一遍,手机上不能白烧。
                            needs_occluder: viz_anchor
                                .is_some(),
                        },
                        size.width,
                        size.height,
                    ) {
                        ui.global::<Viz>()
                            .set_viz_bg(imgs.warp);
                        ui.global::<Viz>()
                            .set_viz_scene(imgs.scene);
                        ui.global::<Viz>()
                            .set_viz_occluder(
                                imgs.occluder,
                            );
                        viz_anchor = imgs.anchor;
                        if let Some((x, y)) = viz_anchor {
                            ui.global::<Viz>()
                                .set_viz_anchor_x(x);
                            ui.global::<Viz>()
                                .set_viz_anchor_y(y);
                        }
                        ui.global::<Viz>()
                            .set_viz_anchor_visible(
                                viz_anchor.is_some(),
                            );
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
