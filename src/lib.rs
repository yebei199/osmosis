//! Minimal Slint app: a tap counter, shared by the Android and desktop builds.
//!
//! Entry points:
//! * Android: [`android_main`] (cdylib, loaded by `MainActivity`)
//! * Desktop dev build: `src/main.rs` (`--features desktop`)

slint::include_modules!();

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use slint::{
    ComponentHandle, RenderingState, Timer, TimerMode,
};

/// Create the window, wire the counter callback, then run the event loop until
/// the window closes. Shared by both entry points.
pub fn run_app() {
    let ui = MainWindow::new()
        .expect("failed to create main window");

    // The count lives here in Rust; the button just asks us to bump it.
    let count = Rc::new(Cell::new(0));
    let weak = ui.as_weak();
    ui.on_bump(move || {
        count.set(count.get() + 1);
        if let Some(ui) = weak.upgrade() {
            ui.set_count(count.get());
        }
    });

    // Frame-rate meter: count frames in the rendering notifier and, twice a
    // second, push the rate to the UI. We request a redraw each frame so the
    // meter stays live instead of reading 0 when the UI is idle.
    // ponytail: continuous redraw pegs the render loop; fine for a dev/study
    // build, drop the request_redraw if battery ever matters on device.
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

/// Android entry point, invoked by the android-activity glue after
/// `MainActivity` loads this library.
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
