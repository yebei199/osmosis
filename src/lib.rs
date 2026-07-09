//! 最小化 Slint 应用:一个点击计数器,Android 与桌面构建共用。
//!
//! 入口点:
//! * Android: [`android_main`](cdylib,由 `MainActivity` 加载)
//! * 桌面开发构建: `src/main.rs`(`--features desktop`)

slint::include_modules!();

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use slint::{
    ComponentHandle, RenderingState, Timer, TimerMode,
};

/// 创建窗口、绑定计数器回调,然后运行事件循环直到窗口关闭。
/// 两个入口点共用此函数。
pub fn run_app() {
    let ui = MainWindow::new()
        .expect("failed to create main window");

    // 计数值保存在 Rust 这一侧;按钮只是请求把它加一。
    let count = Rc::new(Cell::new(0));
    let weak = ui.as_weak();
    ui.on_bump(move || {
        count.set(count.get() + 1);
        if let Some(ui) = weak.upgrade() {
            ui.set_count(count.get());
        }
    });

    // 帧率计:在渲染通知回调里累计帧数,每半秒把帧率推送给 UI。
    // 每帧都主动请求重绘,否则 UI 空闲时读到的会是 0 而不是实际帧率。
    // ponytail: 持续重绘会让渲染循环一直满转,对开发/学习用途没问题;
    // 如果以后在设备上要考虑功耗,再去掉这个 request_redraw。
    let frames = Rc::new(Cell::new(0u32));
    let frames_render = frames.clone();
    let weak_render = ui.as_weak();
    ui.window()
        .set_rendering_notifier(move |state, _| {
            if matches!(
                state,
                RenderingState::BeforeRendering
            ) {
                frames_render.set(frames_render.get() + 1);
                if let Some(ui) = weak_render.upgrade() {
                    ui.window().request_redraw();
                }
            }
        })
        .ok();

    let weak_fps = ui.as_weak();
    let fps_timer = Timer::default();
    fps_timer.start(
        TimerMode::Repeated,
        Duration::from_millis(500),
        move || {
            let counted = frames.replace(0);
            if let Some(ui) = weak_fps.upgrade() {
                ui.set_fps(counted as f32 * 2.0);
            }
        },
    );

    ui.run().expect("event loop failed");
}

#[cfg(all(
    target_os = "android",
    not(feature = "android")
))]
compile_error!(
    "Android builds need the android-activity backend: pass `--no-default-features --features android` (scripts/build-apk.sh does this)"
);

/// Android 入口点,在 `MainActivity` 加载本库后由 android-activity
/// 胶水代码调用。
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("slint_study"),
    );
    log::info!("slint_study starting");

    slint::android::init(app)
        .expect("slint android init failed");
    run_app();
}
