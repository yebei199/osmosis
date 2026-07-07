//! Minimal Slint app: a tap counter, shared by the Android and desktop builds.
//!
//! Entry points:
//! * Android: [`android_main`] (cdylib, loaded by `MainActivity`)
//! * Desktop dev build: `src/main.rs` (`--features desktop`)

slint::include_modules!();

use std::cell::Cell;
use std::rc::Rc;

use slint::ComponentHandle;

/// Create the window, wire the counter callback, then run the event loop until
/// the window closes. Shared by both entry points.
pub fn run_app() {
    let ui = MainWindow::new().expect("failed to create main window");

    // The count lives here in Rust; the button just asks us to bump it.
    let count = Rc::new(Cell::new(0));
    let weak = ui.as_weak();
    ui.on_bump(move || {
        count.set(count.get() + 1);
        if let Some(ui) = weak.upgrade() {
            ui.set_count(count.get());
        }
    });

    ui.run().expect("event loop failed");
}

#[cfg(all(target_os = "android", not(feature = "android")))]
compile_error!(
    "Android builds need the android-activity backend: pass `--no-default-features --features android` (scripts/build-apk.sh does this)"
);

/// Android entry point, invoked by the android-activity glue after
/// `MainActivity` loads this library.
#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(app: slint::android::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("slint_study"),
    );
    log::info!("slint_study starting");

    slint::android::init(app).expect("slint android init failed");
    run_app();
}
