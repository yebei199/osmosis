//! Android 平台入口:被 `NativeActivity` 加载的 cdylib。
//!
//! 职责只有三件:初始化日志、初始化渲染后端、把控制权交给 UI 层。
//!
//! 构建走 cargo-ndk(见 `cargo xtask android`),产物 `libosmosis.so`
//! 由 `MainActivity` 通过 manifest 里的 `android.app.lib_name` 加载。

/// Android 入口点,在 `MainActivity` 加载本库后由 android-activity
/// 胶水代码调用。
///
/// `unsafe(no_mangle)`:关掉名字修饰,把符号以裸名 `android_main` 暴露给链接层,
/// 供 android-activity 的 C 胶水按约定名字 + 签名调用。Rust 2024 要求这类属性
/// 显式标 `unsafe` —— 裸符号可能与其他库撞名,且调用方签名编译器无法校验,契约
/// 由本函数保证。
///
/// `cfg(target_os = "android")`:`slint::android` 只在 android target 下存在,
/// 少了这行,host 上解析 `slint::android::AndroidApp` 就是 E0433。cargo 那边靠
/// default-members 排除本 crate 绕开了,但 IDE 照样按 host cfg 解析这个文件,于是
/// 常年一片红。cfg 让整段在 host 上直接不存在,两边一起治。见 ADR 0003。
#[cfg(target_os = "android")]
mod controls;

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            // logcat 的过滤标签。取 crate 名(即 `[lib] name`,也就是
            // `libosmosis.so` 里的那个名字),而不是另写一遍字面量 ——
            // 改名时 `adb logcat -s <tag>` 才不会跟着失灵。
            .with_tag(env!("CARGO_CRATE_NAME")),
    );
    log::info!("{} starting", env!("CARGO_CRATE_NAME"));

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
    //
    // MCP server 由 init 内部的 set_platform() 顺带起来 —— 这是我们自己 fork
    // (见根 Cargo.toml 的 [patch])里补的:上游只在 backend-selector 路径挂了这个钩子,
    // 而 android::init 直接调 set_platform 把 selector 绕过去了,于是 MCP 永远不启动。
    // 已上报 slint#12446。曾经在这里手动调 i_slint_backend_testing::mcp_server::init()
    // 补刀,fork 之后不再需要,那份内部依赖也一并删了。
    //
    // 注意:MCP 起没起来看不到日志 —— bind 发生在事件循环的异步任务里,失败只 eprintln!
    // 到 stderr,而 Android 把 native 的 stderr 丢进 /dev/null。「装上了却连不上」时
    // 别翻 logcat,直接 curl 那个端口。(踩过一次:manifest 缺 INTERNET 权限导致 bind
    // EACCES,全程零日志。)
    // 媒体控件要 JavaVM,而它只能从 `app` 上取;`init` 会把 `app` 吃掉,
    // 所以先克隆一份留着(`AndroidApp` 内部是 Arc,克隆是廉价的)。
    let media_app = app.clone();
    slint::android::init(app)
        .expect("slint android init failed");

    // 下面这段渲染器分派与 apps/desktop **逐字相同**,故意不抽:签名漂移编译器会一起报错。
    // 曾抽成 render3d::run(scene) 试过,划不来:重复的只有这几行闭包,却要给 render3d
    // 加 ui 依赖、把它从「产帧的 3D 桥」抬成「驱动整个 app」,越过 SRP。入口 crate 本就
    // 同时依赖 ui 与 render3d,是接 seam 的天然组合根。
    let scene = render3d::Scene::new();
    // 导航选中器与播放页 warp 的独立 wgpu pass,复用 scene 的共享 device/queue。
    let mut nav = render3d::NavGlassPass::new(
        scene.device(),
        scene.queue(),
    );
    let mut warp = render3d::WarpPass::new(
        scene.device(),
        scene.queue(),
    );
    let mut btns = render3d::AuroraBtnPass::new(
        scene.device(),
        scene.queue(),
    );
    // 点云与卡墙两个闭包共用同一个 Scene(同线程,RefCell 即可)。
    let scene = std::rc::Rc::new(
        std::cell::RefCell::new(scene),
    );
    let wall_scene = scene.clone();
    // seam:把 ui 的 NavGlassControls / VizControls 平凡拷成 render3d 的镜像参数。
    // 两个闭包分别驱动导航选中器与播放页视觉。
    ui::run_with_renderers(
        move |n| {
            Some(nav.render_frame(&render3d::NavParams {
                strip_w: n.strip_w,
                strip_h: n.strip_h,
                lead: n.lead,
                lag: n.lag,
                drop: n.drop,
                cross: n.cross,
                slot: n.slot,
                thick: n.thick,
                horizontal: n.horizontal,
                ball: n.ball,
                dark: n.dark,
            }))
        },
        move |v, w, h| {
            let (viz_scene, occluder) = scene
                .borrow_mut()
                .render_viz_frame(&render3d::VizFrame {
                    time: v.time,
                    audio: &v.audio,
                    cover: match &v.cover {
                        ui::CoverUpdate::Unchanged => {
                            render3d::CoverUpdate::Unchanged
                        }
                        ui::CoverUpdate::Clear => {
                            render3d::CoverUpdate::Clear
                        }
                        ui::CoverUpdate::Show(c) => {
                            render3d::CoverUpdate::Show(
                                c.width,
                                c.height,
                                c.rgba.as_slice(),
                            )
                        }
                    },
                    pointer: render3d::Pointer {
                        x: v.pointer.x,
                        y: v.pointer.y,
                        down: v.pointer.down,
                        active: v.pointer.active,
                    },
                    preset: v.preset,
                    needs_occluder: v.needs_occluder,
                    width: w,
                    height: h,
                });
            Some(ui::VizImages {
                warp: warp.render_frame(
                    v.time,
                    &v.audio,
                    render3d::WARP_SIDE,
                    render3d::WARP_SIDE,
                ),
                scene: viz_scene,
                occluder,
            })
        },
        // 光带按钮:与上面两条同一个 seam 模式,逐字段平凡拷。
        move |b| {
            btns.render_frame(
                &render3d::AuroraBtnParams {
                    time: b.time,
                    slots: b
                        .slots
                        .iter()
                        .map(|s| render3d::AuroraBtnSlot {
                            w: s.w,
                            h: s.h,
                            radius: s.radius,
                            seed: s.seed,
                            speed: s.speed,
                            amp: s.amp,
                            mode: s.mode,
                            bands: s.bands,
                            variant: s.variant,
                            progress: s.progress,
                            pointer: s.pointer,
                            colors: s.colors,
                        })
                        .collect(),
                },
            )
        },
        // 卡墙(#66):同一个 seam 模式,位姿与封面逐字段平凡拷。
        move |c| {
            Some(wall_scene.borrow_mut().render_wall_frame(
                &render3d::WallFrame {
                    width: c.width,
                    height: c.height,
                    cam: render3d::WallCamera {
                        pan_x: c.pan_x,
                        dolly: c.dolly,
                        perspective: c.perspective,
                    },
                    cards: c
                        .cards
                        .iter()
                        .map(|k| render3d::WallCard {
                            x: k.x,
                            y: k.y,
                            z: k.z,
                            rot_y: k.rot_y,
                            rot_x: k.rot_x,
                            dim: k.dim,
                            size: k.size,
                        })
                        .collect(),
                    covers: c
                        .covers
                        .iter()
                        .map(|k| render3d::WallCover {
                            slot: k.slot,
                            width: k.width,
                            height: k.height,
                            rgba: k.rgba.clone(),
                        })
                        .collect(),
                },
            ))
        },
        // 系统媒体控件:锁屏与通知栏那条播放条由 SystemUI 画,我们只负责报
        // 状态与接按键(见 docs/adr/0020)。`app` 是 JavaVM 的唯一来源。
        move |hooks| controls::start(&media_app, hooks),
    );
}
