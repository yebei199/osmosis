//! Android 平台入口:被 `NativeActivity` 加载的 cdylib。
//!
//! 职责只有三件:初始化日志、初始化渲染后端、把控制权交给 UI 层。
//!
//! 构建走 cargo-ndk(见 `scripts/build-apk.sh`),产物 `libslint_study.so`
//! 由 `MainActivity` 通过 manifest 里的 `android.app.lib_name` 加载。

/// Android 入口点,在 `MainActivity` 加载本库后由 android-activity
/// 胶水代码调用。
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
    ui::run();
}
