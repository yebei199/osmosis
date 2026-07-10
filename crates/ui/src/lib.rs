//! UI 层:界面的声明,以及界面与客户端领域([`app_core`])之间的双向绑定。
//!
//! 依赖方向单向:本 crate 依赖 `app-core`,反向永久禁止。见 `docs/adr/0003`。
//! 各平台入口(`apps/*`)在初始化好渲染后端之后调用 [`run`]。

slint::include_modules!();

use std::cell::RefCell;
use std::rc::Rc;

use app_core::Counter;
use slint::ComponentHandle;

/// 创建窗口、把计数器绑定到 UI,然后运行事件循环直到窗口关闭。
///
/// 调用前平台入口必须已经初始化好 slint 的渲染后端。
pub fn run() {
    let ui = MainWindow::new()
        .expect("failed to create main window");

    // 计数值由 app-core 持有;按钮只是请求把它加一,然后把新值推给 UI。
    let counter = Rc::new(RefCell::new(Counter::default()));
    let weak = ui.as_weak();
    ui.on_bump(move || {
        let mut counter = counter.borrow_mut();
        counter.bump();
        if let Some(ui) = weak.upgrade() {
            ui.set_count(counter.value());
        }
    });

    ui.set_show_fps(cfg!(feature = "debug-fps"));
    // Timer 必须活到事件循环结束,否则会被立即析构、不再触发。
    #[cfg(feature = "debug-fps")]
    let _fps_timer = fps::start(&ui);

    ui.run().expect("event loop failed");
}

/// 帧率计。仅在 `debug-fps` feature 下编译。
///
/// 在渲染通知回调里累计帧数,每个采样周期把帧率推送给 UI。每帧都主动请求重绘,
/// 否则 UI 空闲时读到的会是 0 而不是实际帧率 —— 代价是渲染循环一直满转,
/// 移动端上白耗电。这正是它默认关闭的原因。
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
        let weak_render = ui.as_weak();
        ui.window()
            .set_rendering_notifier(move |state, _| {
                if matches!(
                    state,
                    RenderingState::BeforeRendering
                ) {
                    frames_render
                        .set(frames_render.get() + 1);
                    if let Some(ui) = weak_render.upgrade()
                    {
                        ui.window().request_redraw();
                    }
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
