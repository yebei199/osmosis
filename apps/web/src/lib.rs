//! Web 平台入口:整个应用编译为 wasm,由浏览器加载。
//!
//! 职责与其他平台入口相同:初始化日志、初始化渲染后端、把控制权交给 UI 层。
//! 渲染后端由 slint 的 `backend-winit` + `renderer-femtovg` 静态选定 ——
//! 在 wasm 上它们分别落到 canvas 与 WebGL 上。
//!
//! 注意:slint 的 winit 后端在 wasm 上会寻找页面里 id 为 `canvas` 的
//! `<canvas>` 元素。宿主页面必须提供它。
//!
//! 本 crate 目前只保证**能编译**(`cargo check -p app-web --target
//! wasm32-unknown-unknown`),尚未提供 wasm-bindgen 打包与宿主页面。

use wasm_bindgen::prelude::wasm_bindgen;

/// 浏览器加载 wasm 模块后自动调用。
#[wasm_bindgen(start)]
pub fn start() {
    ui::run();
}
