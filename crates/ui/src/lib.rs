//! UI 层:界面的声明,以及界面与客户端领域([`app_core`])之间的双向绑定。
//!
//! 本 crate 也是**组装点**:它把 [`api`] 的请求函数注入 [`app_core`],
//! 让二者互不相识。依赖方向单向,反向永久禁止。见 `docs/adr/0003`。
//! 各平台入口(`apps/*`)在初始化好渲染后端之后调用 [`run`]。

slint::include_modules!();

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use app_core::{Counter, Health, HealthState};
use slint::{ComponentHandle, Timer, TimerMode};

/// 3D 面板的驱动间隔。约 60fps —— 每帧驱动一次外部渲染器并请求重绘。
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

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
    let _fps_timer = fps::start(&ui);

    ui.run().expect("event loop failed");
}

/// 同 [`run`],但额外每帧驱动一个外部渲染器,把它产出的画面推到 3D 面板。
///
/// `on_frame` 由平台入口提供(见 `render3d`):每帧以当前拖动角度 `(yaw, pitch)`
/// 和面板**物理像素**尺寸 `(w, h)` 调用一次,内部驱动 bevy 前进一帧,返回离屏
/// 纹理包装成的 [`slint::Image`]。角度来自 `rot-yaw`/`rot-pitch`;尺寸取 3D 面板的
/// 逻辑尺寸 `scene-w`/`scene-h` 乘窗口缩放系数,让 bevy 按物理分辨率渲染、HiDPI 上清晰。
/// 仍只传 f32/u32,不涉 bevy / wgpu 类型,依赖边界保持干净分离。
///
/// 仅当 3D 页激活(`render-active`)时才驱动渲染器,其余页跳过 —— 移动端省电。
///
/// 调用前平台入口必须已经用**共享的** wgpu device 配好 Slint 后端,否则 `on_frame`
/// 产出的纹理不属于 Slint 的 device,采样不出来。
pub fn run_with_renderer(
    mut on_frame: impl FnMut(f32, f32, u32, u32) -> slint::Image + 'static,
) {
    let ui = build_ui();
    #[cfg(feature = "debug-fps")]
    let _fps_timer = fps::start(&ui);

    // 每帧:仅 3D 页激活时 → 读角度与面板物理尺寸 → 驱动渲染器一帧 → 推给 UI → 请求重绘。
    // 不请求重绘的话,UI 空闲时 Slint 不会重新采样那张纹理,3D 会定格。
    // Timer 与其捕获的 `on_frame` 必须活到事件循环结束,所以持有到 run() 返回。
    let weak = ui.as_weak();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, FRAME_INTERVAL, move || {
        let Some(ui) = weak.upgrade() else { return };
        if !ui.get_render_active() {
            return;
        }
        let scale = ui.window().scale_factor();
        let w = (ui.get_scene_w() * scale).max(1.0) as u32;
        let h = (ui.get_scene_h() * scale).max(1.0) as u32;
        let frame =
            on_frame(ui.get_rot_yaw(), ui.get_rot_pitch(), w, h);
        ui.set_scene_3d(frame);
        ui.window().request_redraw();
    });

    ui.run().expect("event loop failed");
    drop(timer);
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
                    describe(health.borrow().state()).into(),
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
/// 在渲染通知回调里累计**真实发生的**帧数,每个采样周期算出帧率推给 UI。
/// 刻意不主动请求重绘 —— Slint 是惰性渲染,空闲时本就不重绘,读数会自动趴到
/// ~1(交互/动画时才飙高),这正是诚实的即时帧率,也不会白耗电。3D 页由
/// `run_with_renderer` 的 Timer 每帧驱动重绘,这里自然就读到满帧。
#[cfg(feature = "debug-fps")]
mod fps {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use slint::{
        ComponentHandle, RenderingState, Timer, TimerMode,
    };

    use crate::MainWindow;

    /// 采样周期。帧率 = 本周期内累计的帧数 / 周期秒数。
    const SAMPLE_PERIOD: Duration =
        Duration::from_millis(500);

    /// 启动帧率计。返回的 [`Timer`] 必须由调用方持有到事件循环结束。
    pub(crate) fn start(ui: &MainWindow) -> Timer {
        let frames = Rc::new(Cell::new(0u32));

        let frames_render = frames.clone();
        ui.window()
            .set_rendering_notifier(move |state, _| {
                if matches!(state, RenderingState::BeforeRendering) {
                    frames_render.set(frames_render.get() + 1);
                }
            })
            .ok();

        let weak_fps = ui.as_weak();
        let timer = Timer::default();
        timer.start(
            TimerMode::Repeated,
            SAMPLE_PERIOD,
            move || {
                let counted = frames.replace(0);
                if let Some(ui) = weak_fps.upgrade() {
                    ui.set_fps(
                        counted as f32
                            / SAMPLE_PERIOD.as_secs_f32(),
                    );
                }
            },
        );
        timer
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
            ApiError::Transport("error sending request".to_owned()),
            ApiError::Decode("expected value".to_owned()),
            ApiError::VersionMismatch { expected: 1, actual: 2 },
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
        .chain(
            failures
                .into_iter()
                .map(|err| HealthState::Failed(err.to_string())),
        )
        .collect::<Vec<_>>();

        for state in &states {
            for ch in describe(state).chars().filter(|c| !c.is_ascii()) {
                assert!(
                    face.glyph_index(ch).is_some(),
                    "子集字体缺字形 {ch:?} —— 重跑 `just font-subset` 并把它加进字符集",
                );
            }
        }
    }
}
