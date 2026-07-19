//! Web 平台入口:整个应用编译为 wasm,由浏览器加载。
//!
//! 职责与其他平台入口相同:初始化日志(`console_log`,对应 desktop 的 env_logger、
//! android 的 android_logger)、初始化渲染后端、把控制权交给 UI 层。
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

/// 从 `?tab=` 读启动页签,缺省或非法时回 0。
///
/// 存在的理由是自动化测试:界面整个画在一张 canvas 上,Playwright 没有 DOM 元素可以点,
/// 而按坐标点导航栏在界面一改之后会静默地量错页面。给 3D 页一个可寻址的入口比事后
/// 校对坐标可靠。见 `test/e2e/frame-rate.spec.ts`。
fn initial_tab() -> i32 {
    query_value("tab")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// 取 URL 查询串里某个键的值。
fn query_value(key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    web_sys::window()
        .and_then(|w| w.location().search().ok())?
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix(&prefix))
        .map(str::to_owned)
}

/// 帧率问题的最小复现:纯 Slint + femtovg-wgpu,没有 bevy、没有 render3d、
/// 没有本项目的界面,只有一个永远在跑的动画来驱动持续重绘。
///
/// 存在的理由:`?bevy=off` 下回调只花 1.5ms,却只呈现 58fps,而逐字复刻它的裸 WebGPU
/// 循环能跑 141.8fps(排查过程见 `docs/wasm/frame-rate.md`)。这一档用来判定问题是不是
/// 3D 链路引入的 —— 跑满说明是,跑不满说明是 Slint 自己,同时这段代码就是可以直接
/// 贴给上游的样例。
///
/// 用法:`just web-dev repro`。
#[cfg(feature = "repro")]
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init();

    slint::slint! {
        export component Repro inherits Window {
            background: #0f1117;
            in-out property <bool> flip: false;
            Rectangle {
                y: 100px;
                width: 120px;
                height: 120px;
                // 不用 #4263eb:内联 slint! 宏走 Rust 的词法器,`4263eb` 会被当成
                // 畸形的浮点指数而编译失败。颜色写在 .slint 文件里没有这个问题。
                background: #4263ff;
                // 动画期间 Slint 会持续重绘 —— 这正是本项目 3D 页的重绘节奏来源。
                x: root.flip ? 0px : root.width - 120px;
                animate x { duration: 1800ms; easing: ease-in-out; }
            }
        }
    }

    let ui = Repro::new().expect("建窗口失败");
    // `?notifier=on` 只装一个空的渲染通知,别的都不变。装了它 Slint 就会在每帧开头
    // 多 flush 一次窗口背景 clear(internal/renderers/femtovg/lib.rs:223),
    // 这是 3D 页与最小复现之间仅剩的两处差异之一(另一处是 render3d 建的共享 device)。
    if query_value("notifier").as_deref() == Some("on") {
        ui.window()
            .set_rendering_notifier(|_, _| {})
            .expect("渲染后端必须支持渲染通知");
    }
    // 每 1.8 秒翻一次向,让上面那个动画永远处在运行中。定时器本身不参与帧的节奏。
    let weak = ui.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        core::time::Duration::from_millis(1800),
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_flip(!ui.get_flip());
            }
        },
    );
    ui.set_flip(true);
    // 定时器一旦被丢弃就不再触发,动画会在第一次翻向后停住,页面变成静止的 —— 那样量到的
    // 就不是持续重绘。wasm 上 run() 是否返回不好保证,索性把它漏掉。
    core::mem::forget(timer);
    ui.run().expect("事件循环退出失败");
}

/// 浏览器加载 wasm 模块后自动调用。
///
/// `?notifier=on` 走 [`ui::run_with_renderer`],但交给它一个什么都不画的闭包:于是拿到
/// **真实界面 + 渲染通知驱动的持续重绘,却没有 render3d 建的那个共享 device**。
/// 与 `bevy-3d` 版的 `?tab=2&bevy=off` 相比,差的正好只有那个 device —— 用来把
/// 「界面内容量」与「共享 device」这两个嫌疑分开。
#[cfg(not(any(feature = "bevy-3d", feature = "repro")))]
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init();
    if query_value("notifier").as_deref() == Some("on") {
        ui::run_with_renderer(initial_tab(), |_, _, _| slint::Image::default());
    } else {
        ui::run();
    }
}

/// 浏览器加载 wasm 模块后自动调用(bevy-3d 版,async 由 wasm-bindgen 驱动)。
#[cfg(feature = "bevy-3d")]
#[wasm_bindgen(start)]
pub async fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init();
    // Scene 配置 Slint 的 wgpu 后端,必须在 ui 建窗口之前完成 —— 故先 await 它。
    // 下面这段 3D 分派与 apps/desktop、apps/android 的两份**逐字相同**(仅构造是
    // async),故意不抽,理由见 apps/android/src/lib.rs。改动时记得同步另两端。
    let mut scene = render3d::Scene::new_async().await;
    // `?bevy=off` 跳过驱动渲染器,但**照常请求重绘** —— 于是界面仍以同样的节奏画,只是
    // 不含 bevy 那一份工作。用来把 bevy 的开销与 Slint 自己的分开。直接不装渲染通知是
    // 不行的:那样界面会停在原地,量到的就不是同一件事了。
    let drive_renderer = query_value("bevy").as_deref() != Some("off");
    // seam:把 ui 的 SceneControls 平凡拷成 render3d 的 SceneParams(见 SceneParams 注释)。
    ui::run_with_renderer(initial_tab(), move |c, w, h| {
        if !drive_renderer {
            return slint::Image::default();
        }
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
