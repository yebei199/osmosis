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
