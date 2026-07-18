//! UI 层:界面的声明,以及界面与客户端领域([`app_core`])之间的双向绑定。
//!
//! 本 crate 也是**组装点**:它把 [`api`] 的请求函数注入 [`app_core`],
//! 让二者互不相识。依赖方向单向,反向永久禁止。见 `docs/adr/0003`。
//! 各平台入口(`apps/*`)在初始化好渲染后端之后调用 [`run`]。

slint::include_modules!();

mod scene_params;
pub use scene_params::SceneControls;

use std::cell::RefCell;
use std::rc::Rc;

use app_core::{Counter, Health, HealthState};
use slint::{ComponentHandle, RenderingState};

/// 创建窗口并完成所有领域状态绑定。[`run`] 与 [`run_with_renderer`] 的公共前半段。
///
/// 调用前平台入口必须已经初始化好 slint 的渲染后端。
fn build_ui() -> MainWindow {
    let ui = MainWindow::new()
        .expect("failed to create main window");

    bind_counter(&ui);
    bind_health(&ui);

    ui.set_show_fps(cfg!(feature = "debug-fps"));
    ui.set_platform(platform_name().into());
    // 开局停在哪一页。只为验证:`just shot 420 2` 能直接截到 3D 页,不必再靠 MCP 模拟点击
    // (那条路上有一串静默失败的坑,见 AGENTS.md)。没设或设歪了就走默认的 Home。
    if let Ok(tab) = std::env::var("SLINT_STUDY_TAB")
        && let Ok(tab) = tab.parse::<i32>()
    {
        ui.set_current_tab(tab.clamp(0, 2));
    }
    ui
}

/// 创建窗口、绑定领域状态,然后运行事件循环直到窗口关闭。
///
/// 各平台入口在初始化好渲染后端后调用。不带 3D 的普通路径(web / ios,以及
/// 未启用 3D 的桌面)走这里。
pub fn run() {
    let ui = build_ui();
    // Timer 必须活到事件循环结束,否则会被立即析构、不再触发。
    #[cfg(feature = "debug-fps")]
    let _fps_timer = {
        let (frames, timer) = fps::start(&ui);
        // 无 3D 的路径上没人装渲染通知,帧计数在这里自己接。
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
    };

    ui.run().expect("event loop failed");
}

/// 同 [`run`],但额外每帧驱动一个外部渲染器,把它产出的画面推到 3D 面板。
///
/// `on_frame` 由平台入口提供(见 `render3d`):每帧以当前 [`SceneControls`] 和面板
/// **物理像素**尺寸 `(w, h)` 调用一次,内部驱动 bevy 前进一帧,返回离屏纹理包装成的
/// [`slint::Image`]。控制量在这里组装:朝向来自 `rot-yaw`/`rot-pitch`,场景与热调参数
/// 来自 `scene-index` 与四个 LineEdit 文本——原始文本在**信任边界**用 [`scene_params`]
/// 解析+clamp,非法/越界退回上一个好值(持久保存在 `controls` 里)。尺寸取 3D 面板逻辑
/// 尺寸乘窗口缩放系数,让 bevy 按物理分辨率渲染、HiDPI 上清晰。
///
/// 只传 POD 的 `SceneControls`,不涉 bevy / wgpu 类型,依赖边界保持干净分离。
/// 仅当 3D 页激活(`render-active`)时才驱动渲染器,其余页跳过 —— 移动端省电。
///
/// 调用前平台入口必须已经用**共享的** wgpu device 配好 Slint 后端,否则 `on_frame`
/// 产出的纹理不属于 Slint 的 device,采样不出来。
pub fn run_with_renderer(
    mut on_frame: impl FnMut(
        &SceneControls,
        u32,
        u32,
    ) -> slint::Image
    + 'static,
) {
    let ui = build_ui();
    #[cfg(feature = "debug-fps")]
    let (fps_frames, _fps_timer) = fps::start(&ui);

    // 一帧的account:回调里(我们:组装参数 + 驱动渲染器)与回调外(Slint 重绘整个
    // 界面 + 浏览器合成/呈现)各占多少。web 上帧率被砍半时,只有这个比值能说明该往
    // 哪边使劲 —— render3d 自己的计时只覆盖回调内那一段,回调外从没被量过。
    let mut frame_acct = FrameAccounting::default();

    // 每帧:仅 3D 页激活时 → 组装 SceneControls 与面板物理尺寸 → 驱动渲染器一帧 → 推给
    // UI → 请求重绘。不请求重绘的话,Slint 惰性渲染,下一帧通知不会来,3D 会定格。
    // controls 跨帧持久:解析失败时各字段退回上一个好值。初值须与 app.slint 的默认属性一致。
    let weak = ui.as_weak();
    let mut controls = SceneControls {
        scene_id: 0,
        yaw: 0.0,
        pitch: 0.0,
        count: 8,
        color_rgb: 0x4a6bff,
        spin_speed: 0.0,
        spacing: 1.5,
        // 每帧从 .slint 量出来重算,初值无所谓。
        glass: scene_params::GlassRect::default(),
    };
    // 帧驱动挂在**渲染通知**上,不是定时器。理由是 wasm:浏览器主线程唯一,合成、
    // 派发输入、跑 wasm 全挤在上面,固定间隔的 setTimeout 与合成器各跑各的 —— 间隔调小
    // 会把主线程占死(1ms 实测 rAF 掉到 2~8 次/秒,整个界面卡住),调大又硬性设了帧率
    // 上限(16ms 压在 ~62fps,实测只有 40)。渲染通知由 Slint 真正的重绘周期派发:
    // wasm 上是 requestAnimationFrame(合成完才给下一次,天然不会饿死主线程,且按显示器
    // 刷新率走),原生上是 vsync。两端都不再有人为的帧率上限,也不再有定时器与合成的错拍。
    //
    // 回调里改 `scene-3d` 属性会标脏,画面在**下一帧**生效,故恒差一帧 —— 这是 3D 面板,
    // 一帧延迟看不出来,不为此发明双缓冲。
    // 回调与其捕获的 `on_frame` 由窗口持有,活到事件循环结束。
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
            #[cfg(feature = "debug-fps")]
            fps_frames.set(fps_frames.get() + 1);
            frame_acct.begin_frame();

            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_render_active() {
                return;
            }
            let scale = ui.window().scale_factor();
            let w =
                (ui.get_scene_w() * scale).max(1.0) as u32;
            let h =
                (ui.get_scene_h() * scale).max(1.0) as u32;

            controls.scene_id = ui.get_scene_index();
            controls.yaw = ui.get_rot_yaw();
            controls.pitch = ui.get_rot_pitch();
            controls.count = scene_params::parse_count(
                ui.get_count_text().as_str(),
                controls.count,
            );
            controls.color_rgb =
                scene_params::parse_hex_rgb(
                    ui.get_color_text().as_str(),
                    controls.color_rgb,
                );
            controls.spin_speed = scene_params::parse_speed(
                ui.get_speed_text().as_str(),
                controls.spin_speed,
            );
            controls.spacing = scene_params::parse_spacing(
                ui.get_spacing_text().as_str(),
                controls.spacing,
            );
            // 工具条几何量:逻辑像素 × 缩放系数 → 物理像素,与离屏纹理同一坐标系。
            // 唯一真相在 app.slint 的 glass-* 属性,这里只做单位换算。
            controls.glass = scene_params::GlassRect {
                x: ui.get_glass_x() * scale,
                y: ui.get_glass_y() * scale,
                w: ui.get_glass_w() * scale,
                h: ui.get_glass_h() * scale,
                radius: ui.get_glass_r() * scale,
            };

            let frame = on_frame(&controls, w, h);
            ui.set_scene_3d(frame);
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

/// 计数值由 app-core 持有;按钮只是请求把它加一,然后把新值推给 UI。
fn bind_counter(ui: &MainWindow) {
    let counter = Rc::new(RefCell::new(Counter::default()));
    let weak = ui.as_weak();
    ui.on_bump(move || {
        let mut counter = counter.borrow_mut();
        counter.bump();
        if let Some(ui) = weak.upgrade() {
            ui.set_count(counter.value());
        }
    });
}

/// 把 `GET /health` 接到"查询服务端"按钮上。
///
/// `slint::spawn_local` 在 slint 自己的事件循环上驱动 future,native 与 wasm
/// 通用 —— 所以这里没有 `#[cfg]`。native 上真正的 IO 在 `api` 内部的后台
/// tokio runtime 里跑,回到这里时已经是 UI 线程。见 `docs/adr/0002`。
fn bind_health(ui: &MainWindow) {
    let health = Rc::new(RefCell::new(Health::default()));
    let weak = ui.as_weak();

    ui.on_check_health(move || {
        let Some(ui) = weak.upgrade() else { return };
        // spawn_local 的 future 要到下一轮事件循环才开始跑,而 Loading 需要
        // 立刻显示出来。ponytail: 直接在这里推一次文案,不为此发明一套订阅机制。
        ui.set_health_text(
            describe(&HealthState::Loading).into(),
        );

        let health = health.clone();
        let weak = ui.as_weak();
        slint::spawn_local(async move {
            app_core::refresh(&health, api::health).await;
            if let Some(ui) = weak.upgrade() {
                ui.set_health_text(
                    describe(health.borrow().state())
                        .into(),
                );
            }
        })
        .expect("event loop must be running");
    });

    ui.set_health_text(describe(&HealthState::Idle).into());
}

/// 把领域状态翻译成一行人类可读的文案。
fn describe(state: &HealthState) -> String {
    match state {
        HealthState::Idle => {
            format!("未查询 · {}", api::base_url())
        }
        HealthState::Loading => "查询中…".to_owned(),
        HealthState::Loaded(dto) => format!(
            "服务端 {} · 协议 v{}",
            dto.status, dto.protocol_version
        ),
        HealthState::Failed(message) => {
            format!("失败: {message}")
        }
    }
}

/// 帧率计。仅在 `debug-fps` feature 下编译。
///
/// 帧数由调用方在渲染通知回调里累加**真实发生的**帧,每个采样周期算出帧率推给 UI。
/// 计数器不在这里接进渲染通知:一个窗口只能装一个通知回调,而 3D 路径要拿它当帧驱动
/// (见 [`run_with_renderer`](crate::run_with_renderer))—— 两边都装的话后者会顶掉前者,
/// 读数静默归零。故本模块只出计数器和采样定时器,由谁装通知、在哪儿 `bump` 交给调用方。
///
/// 刻意不主动请求重绘 —— Slint 是惰性渲染,空闲时本就不重绘,读数会自动趴到
/// ~1(交互/动画时才飙高),这正是诚实的即时帧率,也不会白耗电。3D 页每帧自请求重绘,
/// 这里自然就读到满帧。
#[cfg(feature = "debug-fps")]
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

#[cfg(test)]
mod tests {
    use api::ApiError;
    use app_core::HealthDto;

    use super::*;

    /// 生产侧由 `app.slint` 的 `import` 内嵌;测试自己再读一份来查字形覆盖。
    const CJK_SUBSET: &[u8] =
        include_bytes!("../fonts/cjk-subset.ttf");

    /// 子集字体必须覆盖 [`describe`] 可能吐出的每一个非 ASCII 字符。
    ///
    /// 字符集不是手抄的:直接遍历 `describe` 在各状态下的真实输出。新增中文
    /// 文案而忘了重新裁字体时,这里就会红。
    #[test]
    fn describe_only_uses_subset_glyphs() {
        let face = ttf_parser::Face::parse(CJK_SUBSET, 0)
            .expect("子集字体应能被解析");

        // Failed 里的 message 是 `ApiError` 的 Display,自带中文 —— 这三个变体
        // 必须逐一走到,否则漏字要等到线上点出错误才看得见。
        let failures = [
            ApiError::Transport(
                "error sending request".to_owned(),
            ),
            ApiError::Decode("expected value".to_owned()),
            ApiError::VersionMismatch {
                expected: 1,
                actual: 2,
            },
        ];

        let states = [
            HealthState::Idle,
            HealthState::Loading,
            // status 由服务端给,恒为 ASCII 的 "ok"。
            HealthState::Loaded(HealthDto {
                status: "ok".to_owned(),
                protocol_version: 1,
            }),
        ]
        .into_iter()
        .chain(failures.into_iter().map(|err| {
            HealthState::Failed(err.to_string())
        }))
        .collect::<Vec<_>>();

        for state in &states {
            for ch in describe(state)
                .chars()
                .filter(|c| !c.is_ascii())
            {
                assert!(
                    face.glyph_index(ch).is_some(),
                    "子集字体缺字形 {ch:?} —— 重跑 `just font-subset` 并把它加进字符集",
                );
            }
        }
    }
}
