//! 桌面平台入口(linux / windows / macOS)。
//!
//! 职责:初始化日志、初始化渲染后端、把控制权交给 UI 层。
//! 默认渲染后端由 Cargo.toml 里 slint 的 `backend-winit` feature 静态选定。
//!
//! 开启 `bevy-3d` feature 时,改由 `render3d` 先用共享 wgpu device 配好 Slint 的
//! wgpu 后端(必须在建窗口前),再把每帧驱动 bevy 的闭包交给 `ui::run_with_renderer`。
//!
//! 运行:`cargo run -p app-desktop`
//! 带 3D:`nix-shell render3d.nix --run "cargo run -p app-desktop --features bevy-3d"`

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("info"),
    )
    .init();

    // Scene::new 会配置 Slint 的 wgpu 后端,必须在 ui 建窗口之前发生 —— 故先建它。
    // 下面这段与 apps/android **逐字相同**,故意不抽(理由见 android 那边)。web 端只差一处:
    // 它是紧凑版式、没有宽版式侧栏,故仍走 ui::run_with_renderer(不带导航选中器);
    // desktop/android 走 run_with_renderers 多驱动一个 NavGlassPass。改 bevy 分支时同步三端。
    #[cfg(feature = "bevy-3d")]
    {
        let mut scene = render3d::Scene::new();
        // 导航选中器的独立 wgpu pass,复用 scene 的共享 device/queue(必须在 scene 被移进闭包前取)。
        let mut nav = render3d::NavGlassPass::new(
            scene.device(),
            scene.queue(),
        );
        // seam:把 ui 的 SceneControls / NavGlassControls 平凡拷成 render3d 的镜像结构体
        // (见 SceneParams 注释)。两个闭包分别驱动 3D 面板与导航选中器。
        ui::run_with_renderers(
            0,
            move |c, w, h| {
                scene.render_frame(
                    &render3d::SceneParams {
                        scene_id: c.scene_id,
                        yaw: c.yaw,
                        pitch: c.pitch,
                        count: c.count,
                        color_rgb: c.color_rgb,
                        spin_speed: c.spin_speed,
                        spacing: c.spacing,
                        glass: render3d::GlassRect {
                            x: c.glass.x,
                            y: c.glass.y,
                            w: c.glass.w,
                            h: c.glass.h,
                            radius: c.glass.radius,
                        },
                    },
                    w,
                    h,
                )
            },
            move |n| {
                Some(nav.render_frame(
                    &render3d::NavParams {
                        strip_w: n.strip_w,
                        strip_h: n.strip_h,
                        lead_y: n.lead_y,
                        lag_y: n.lag_y,
                        slot_h: n.slot_h,
                    },
                ))
            },
        );
    }
    #[cfg(not(feature = "bevy-3d"))]
    ui::run();
}
