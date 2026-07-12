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

    // APK 由系统启动,拿不到运行时环境变量(桌面那边是 `SLINT_MCP_PORT=8090 cargo run`)。
    // 故把构建期的端口烧进二进制,再在这里塞回进程环境 —— 必须赶在下面 android::init
    // 之前,后端初始化时才会读到它并起 MCP server。端口真源见 justfile 的 mcp_port。
    // unsafe:set_var 要求调用时无其他线程在读写环境;此处是 android_main 头部,
    // slint 与 bevy 都还没起线程,契约成立。
    #[cfg(feature = "mcp")]
    if let Some(port) = option_env!("SLINT_MCP_PORT") {
        unsafe {
            std::env::set_var("SLINT_MCP_PORT", port)
        };
    }

    // 必须先 init 设好 android 平台;render3d::Scene::new 里的
    // require_wgpu_29(Manual).select() 是把共享 device 转发给这个已设好的平台,
    // 顺序反了就没平台可转发。见 i-slint-backend-selector 的 android 分支。
    slint::android::init(app)
        .expect("slint android init failed");

    // 上游在 android 上没接 MCP:桌面走 i-slint-backend-selector,后者设完 platform 会顺手
    // 调 init_testing_backends() -> mcp_server::init();而 slint::android::init 直接调
    // platform::set_platform,把 selector 整个绕过去了,那个钩子永远不触发。故自己补一刀 ——
    // 必须在 set_platform 之后,init 内部要拿 SlintContext 去 spawn_local 起 HTTP server。
    // 代价是直依赖 slint 的内部 crate(版本对齐的坑见 Cargo.toml)。上游哪天接上了,删掉这段。
    //
    // 只警告不 panic:MCP 是调试设施,起不来不该把整个 app 拖垮。
    // 注意 init() 返回 Ok 不代表 server 真的起来了 —— 它只是把 run_server 挂上事件循环,
    // 真正的 bind 发生在之后的异步任务里,失败只会 eprintln! 到 stderr,而 Android 把 native
    // 的 stderr 丢进 /dev/null。所以「装上了却连不上」时别看 logcat,直接 curl 那个端口。
    // (踩过一次:manifest 缺 INTERNET 权限导致 bind EACCES,全程零日志。)
    #[cfg(feature = "mcp")]
    if let Err(e) = i_slint_backend_testing::mcp_server::init() {
        log::warn!("MCP server init failed: {e:?}");
    }

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
        // seam:把 ui 的 SceneControls 平凡拷成 render3d 的 SceneParams(见 SceneParams 注释)。
        ui::run_with_renderer(move |c, w, h| {
            scene.render_frame(
                &render3d::SceneParams {
                    scene_id: c.scene_id,
                    yaw: c.yaw,
                    pitch: c.pitch,
                    count: c.count,
                    color_rgb: c.color_rgb,
                    spin_speed: c.spin_speed,
                    spacing: c.spacing,
                },
                w,
                h,
            )
        });
    }
    #[cfg(not(feature = "bevy-3d"))]
    ui::run();
}
