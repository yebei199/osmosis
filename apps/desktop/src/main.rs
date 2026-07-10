//! 桌面平台入口(linux / windows / macOS)。
//!
//! 职责只有三件:初始化日志、初始化渲染后端、把控制权交给 UI 层。
//! 渲染后端由 Cargo.toml 里 slint 的 `backend-winit` feature 静态选定,
//! 无需在此显式初始化。
//!
//! 运行:`cargo run -p app-desktop`

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("info"),
    )
    .init();
    ui::run();
}
