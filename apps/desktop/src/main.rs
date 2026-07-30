//! 桌面平台入口(linux / windows / macOS)。
//!
//! 职责:初始化日志、初始化渲染后端、把控制权交给 UI 层。
//!
//! `render3d` 先用共享 wgpu device 配好 Slint 的 wgpu 后端(必须在建窗口前),
//! 再把每帧驱动 bevy 的闭包交给 `ui::run_with_renderers`。
//!
//! 运行:`nix-shell slint.nix --run "cargo run -p app-desktop"`

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("info"),
    )
    .init();

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
                .render_viz_frame(
                    v.time,
                    &v.audio,
                    v.cover.as_ref().map(|c| {
                        (c.width, c.height, c.rgba.as_slice())
                    }),
                    w,
                    h,
                );
            Some(ui::VizImages {
                warp: warp
                    .render_frame(v.time, &v.audio, w, h),
                scene: viz_scene,
                occluder,
            })
        },
    );
}
