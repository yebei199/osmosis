//! Web 平台入口:整个应用编译为 wasm,由浏览器加载。
//!
//! 职责与其他平台入口相同:初始化日志(`console_log`,对应 desktop 的 env_logger、
//! android 的 android_logger)、初始化渲染后端、把控制权交给 UI 层。
//! 渲染后端由 slint 的 `backend-winit` + `renderer-femtovg` 静态选定 ——
//! 在 wasm 上它们分别落到 canvas 与 WebGL 上。
//!
//! **不带 bevy**:`render3d` 只服务桌面与 android,由 `xtask boundaries` 守住这条边界。
//! 播放页覆层因此退回没有粒子与 warp 的形态(见 `ui::VizImages`),`.slint` 里零平台判断。
//! 曾经有过一个 `bevy-3d` feature,那时 web 上有个 3D 演示页可看;演示页删掉之后它
//! 无图可渲 —— wasm 没有原生音频栈(`ui::viz::Source` 恒为 `None`),播放页视觉在
//! web 上永远不会开门。等 web 的播放链路通了再接。
//!
//! 注意:slint 的 winit 后端在 wasm 上会寻找页面里 id 为 `canvas` 的
//! `<canvas>` 元素。宿主页面必须提供它。
//!
//! panic 钩子:wasm 默认把 panic 报成一句没有信息量的 `RuntimeError: unreachable`,
//! 挂上 `console_error_panic_hook` 才能在控制台看到 panic 消息本身。

use wasm_bindgen::prelude::wasm_bindgen;

/// 浏览器加载 wasm 模块后自动调用。
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init();
    ui::run();
}
