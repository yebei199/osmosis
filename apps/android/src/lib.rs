//! Android 平台入口:被 `NativeActivity` 加载的 cdylib。
//!
//! 职责只有三件:初始化日志、初始化渲染后端、把控制权交给 UI 层。
//!
//! 构建走 cargo-ndk(见 `cargo xtask android`),产物 `libslint_study.so`
//! 由 `MainActivity` 通过 manifest 里的 `android.app.lib_name` 加载。

/// Android 入口点,在 `MainActivity` 加载本库后由 android-activity
/// 胶水代码调用。
///
/// `unsafe(no_mangle)`:关掉名字修饰,把符号以裸名 `android_main` 暴露给链接层,
/// 供 android-activity 的 C 胶水按约定名字 + 签名调用。Rust 2024 要求这类属性
/// 显式标 `unsafe` —— 裸符号可能与其他库撞名,且调用方签名编译器无法校验,契约
/// 由本函数保证。
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("slint_study"),
    );
    log::info!("slint_study starting");

    // 必须先 init 设好 android 平台;render3d::Scene::new 里的
    // require_wgpu_29(Manual).select() 是把共享 device 转发给这个已设好的平台,
    // 顺序反了就没平台可转发。见 i-slint-backend-selector 的 android 分支。
    slint::android::init(app)
        .expect("slint android init failed");

    // 下面这段 3D 分派与 apps/desktop/src/main.rs 里的一份**逐字相同**,故意不抽:
    // 只两处、且签名漂移编译器会两边一起报错。若给某一端的 bevy 分支加初始化步骤,
    // 记得同步另一端。
    // 曾抽成 render3d::run(scene) 试过,划不来:重复的只有这一行闭包,却要给 render3d
    // 加 ui 依赖、把它从「产帧的 3D 桥」抬成「驱动整个 app」,越过 SRP。入口 crate 本就
    // 同时依赖 ui 与 render3d,是接 seam 的天然组合根。等 web-3d 随 slint#11580 复活、
    // 成三处且构造分叉(web 异步)时再抽不迟。
    #[cfg(feature = "bevy-3d")]
    {
        let mut scene = render3d::Scene::new();
        ui::run_with_renderer(move |yaw, pitch, w, h| {
            scene.render_frame(yaw, pitch, w, h)
        });
    }
    #[cfg(not(feature = "bevy-3d"))]
    ui::run();
}
