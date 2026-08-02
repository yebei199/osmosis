//! 桌面平台入口(linux / windows / macOS)。
//!
//! 职责:初始化日志、初始化渲染后端、把控制权交给 UI 层。
//!
//! `render3d` 先用共享 wgpu device 配好 Slint 的 wgpu 后端(必须在建窗口前),
//! 再把每帧驱动 bevy 的闭包交给 `ui::run_with_renderers`。
//!
//! 运行:`nix-shell slint.nix --run "cargo run -p app-desktop"`

mod single_instance;

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("info"),
    )
    .init();

    // 启动锁在**一切之前**:两个实例会抢同一块声卡各放各的歌,而 MCP 那个固定
    // 端口只有先起来的抢得到 —— 调试时连上的可能是上次忘了关的那个实例。
    // `_lock` 要活到 main 结束,丢掉它就等于开门。
    let Ok(_lock) = single_instance::claim() else {
        eprintln!(
            "已经有一个 osmosis-desktop 在跑了。先关掉它,或者 just desktop-kill。"
        );
        std::process::exit(1);
    };

    // Scene::new 会配置 Slint 的 wgpu 后端,必须在 ui 建窗口之前发生 —— 故先建它。
    // 下面这段与 apps/android **逐字相同**,故意不抽(理由见 android 那边)。
    let mut scene = render3d::Scene::new();
    // 导航选中器与播放页 warp 的独立 wgpu pass,复用 scene 的共享 device/queue。
    let mut nav = render3d::NavGlassPass::new(
        scene.device(),
        scene.queue(),
    );
    let mut warp = render3d::WarpPass::new(
        scene.device(),
        scene.queue(),
    );
    // seam:把 ui 的 NavGlassControls / VizControls 平凡拷成 render3d 的镜像参数。
    // 两个闭包分别驱动导航选中器与播放页视觉。
    ui::run_with_renderers(
        move |n| {
            Some(nav.render_frame(&render3d::NavParams {
                strip_w: n.strip_w,
                strip_h: n.strip_h,
                lead_y: n.lead_y,
                lag_y: n.lag_y,
                slot_h: n.slot_h,
            }))
        },
        move |v, w, h| {
            let (viz_scene, occluder) = scene
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
    );

    // 事件循环已经返回,进程该走了 —— 但**不能让它自然返回**,否则 debug 构建关窗必崩
    // (exit 134,见 issue #15)。
    //
    // 链路:Slint 把它的 `SlintContext` 放在一个 thread_local 里,窗口、渲染器、以及我们
    // 挂上去的渲染通知闭包(连同 bevy `Scene` 与两条 wgpu pass)都吊在它下面。main 返回后
    // glibc 跑 `__call_tls_dtors`,这棵图才开始析构,于是 `wgpu::Queue::drop` 落在 TLS
    // 析构阶段;它要取 `SnatchLock`,而那把锁的递归检测(wgpu-core 29 `snatch.rs`,
    // `#[cfg(debug_assertions)]`)自己也存在一个 thread_local 里,**那时已经析构了** ——
    // 读它 panic,drop 里 panic 直接 abort。两个 TLS 同在主线程,析构顺序不定,谁先没谁背锅。
    //
    // 两条走不通的路,别再试:
    //
    // - 提前放掉自己那份 wgpu 句柄。Slint 的渲染器还持着一份,最后一次释放仍落在 TLS 里。
    // - `std::process::exit`。它调的是 libc `exit()`,退出处理器与 TLS 析构器照样跑 ——
    //   实测退出码仍是 134,与不加时一字不差。
    //
    // 所以要 `_exit`:直接进 `exit_group`,谁的析构都不跑。代价是析构函数不执行 ——
    // 而此刻要还的只有 GPU 与音频设备,内核回收得比我们干净,且全仓没有任何落盘路径
    // (核对过 crates/ 与 apps/,没有 `fs::write` / `File::create`),没有东西要 flush。
    // 日志也不丢:env_logger 写的是 stderr,不带缓冲。
    //
    // release 构建其实不受影响(那段递归检测只在 debug 下编进去),但开发全程跑的是
    // debug,core dump 会污染崩溃统计。
    //
    // 回归验收:`just desktop-exit-check`。
    #[cfg(unix)]
    // SAFETY: `_exit` 无条件终止进程,没有可被破坏的不变量;此刻也没有别的线程在等我们。
    unsafe {
        libc::_exit(0)
    }
    #[cfg(not(unix))]
    std::process::exit(0);
}
