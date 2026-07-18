//! Web 平台入口:整个应用编译为 wasm,由浏览器加载。
//!
//! 职责与其他平台入口相同:初始化日志、初始化渲染后端、把控制权交给 UI 层。
//! 渲染后端由 slint 的 `backend-winit` + `renderer-femtovg` 静态选定 ——
//! 在 wasm 上它们分别落到 canvas 与 WebGL 上。开启 `wgpu` feature 时改走
//! femtovg-wgpu 渲染器(WebGPU,不支持则回退 wgpu 的 WebGL 后端)。
//!
//! 开启 `bevy-3d` feature 时,入口变成 async:先 await 共享 wgpu device
//! (浏览器的 WebGPU 初始化是真 Promise,主线程不许阻塞),配好 Slint 的
//! wgpu 后端,再进 UI 主循环。仅支持 WebGPU,没有的浏览器在此 panic
//! (控制台可见),不做 WebGL 降级。
//!
//! 注意:slint 的 winit 后端在 wasm 上会寻找页面里 id 为 `canvas` 的
//! `<canvas>` 元素。宿主页面必须提供它。
//!
//! panic 钩子:wasm 默认把 panic 报成一句没有信息量的 `RuntimeError: unreachable`,
//! 挂上 `console_error_panic_hook` 才能在控制台看到 panic 消息本身
//! (「找不到可用的 wgpu adapter」这类)。

use wasm_bindgen::prelude::wasm_bindgen;

/// 浏览器加载 wasm 模块后自动调用。
#[cfg(not(feature = "bevy-3d"))]
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    ui::run();
}

/// 浏览器加载 wasm 模块后自动调用(bevy-3d 版,async 由 wasm-bindgen 驱动)。
#[cfg(feature = "bevy-3d")]
#[wasm_bindgen(start)]
pub async fn start() {
    console_error_panic_hook::set_once();
    // Scene 配置 Slint 的 wgpu 后端,必须在 ui 建窗口之前完成 —— 故先 await 它。
    // 下面这段 3D 分派与 apps/desktop、apps/android 的两份**逐字相同**(仅构造是
    // async),故意不抽,理由见 apps/android/src/lib.rs。改动时记得同步另两端。
    let mut scene = render3d::Scene::new_async().await;
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
    });
}
