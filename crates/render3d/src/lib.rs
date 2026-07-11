//! 3D 桥:用 **bevy** 在**共享的** wgpu-29 device 上离屏渲染,产出一张 [`slint::Image`]
//! 交给 UI 层合成。只有桌面 / android 入口依赖本 crate,web / ios 永不碰它 ——
//! 由 `xtask boundaries` 守住这条边界。
//!
//! 架构约束(见计划 `bevy-serialized-dove`):
//! - device 由本 crate 自建(Manual),同一套 instance/adapter/device/queue
//!   既注入 Slint 的 `require_wgpu_29`,也注入 bevy 的 `RenderCreation::manual`。
//! - bevy 主线程无头运行,禁 `bevy_winit`,由 Slint 的 `Timer` 每帧驱动 `app.update()`,
//!   绝不调 `App::run()` —— 事件循环永远归 Slint。
//! - bevy 与 Slint 共享同一 wgpu 大版本(现为 29),纹理类型才是同一个,才能被 Slint 采样。
//!
//! 用法(见 `apps/desktop`):先 [`Scene::new`](Scene::new) —— 它顺带配好 Slint 的 wgpu 后端,
//! 必须在建窗口**之前**调 —— 再把 `move || scene.render_frame()` 交给 `ui::run_with_renderer`。

use std::sync::Arc;

use bevy::prelude::*;
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。
use bevy::camera::RenderTarget;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::render::RenderApp;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::TextureFormat;
use bevy::render::renderer::{
    RenderAdapter, RenderAdapterInfo, RenderDevice, RenderInstance, RenderQueue, WgpuWrapper,
};
use bevy::render::settings::RenderCreation;
use bevy::render::texture::GpuImage;
use bevy::render::RenderPlugin;
use bevy::window::{ExitCondition, WindowPlugin};
use slint::wgpu_29::wgpu;

/// bevy 与 slint 共享的 `wgpu::Texture` 类型别名(经 slint 的 wgpu_29 再导出,与 bevy 同一份 crate)。
pub type SharedTexture = wgpu::Texture;

/// 离屏画面尺寸。固定分辨率,Slint 侧按面板大小缩放(见计划:先不做动态 resize)。
const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;

/// 标记要自转的实体。
#[derive(Component)]
struct Spin;

/// 一个自持的 bevy 离屏渲染场景。
///
/// 持有 bevy `App`、离屏目标图的句柄,以及一次性包装好的 [`slint::Image`]。
/// 整个对象只在 Slint 的主线程上被 [`render_frame`](Scene::render_frame) 驱动。
pub struct Scene {
    app: App,
    target: Handle<Image>,
    /// 首帧渲染出纹理后才 `Some`。之前返回空图,UI 的 `scene-3d.width > 0` 守卫会让面板暂不显示。
    image: Option<slint::Image>,
    /// 已驱动的帧数。仅用于诊断:纹理迟迟不就绪时给一次告警。
    frames: u32,
}

impl Scene {
    /// 自建共享 wgpu device、配置 Slint 后端、搭好 bevy 无头渲染场景。
    ///
    /// **必须在创建任何 Slint 窗口之前调用** —— `require_wgpu_29(...).select()` 是全局的,
    /// 一旦窗口建出来就晚了。
    pub fn new() -> Self {
        // 1) 自建一套 wgpu。经 slint 的 wgpu_29 再导出拿到 wgpu,保证和 bevy 是同一份 crate。
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = bevy::tasks::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            },
        ))
        .expect("找不到可用的 wgpu adapter");
        // 用 adapter 支持的全部 features/limits 建 device,确保 bevy 想要什么都有。
        let (device, queue) = bevy::tasks::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("render3d-shared"),
                required_features: adapter.features(),
                required_limits: adapter.limits(),
                // adapter.features() 含实验特性(mesh shader / ray query 等),要请求它们
                // 就必须同时启用 experimental_features,否则 request_device 报
                // ExperimentalFeaturesNotEnabled。与 bevy 内部建 device 的做法一致。
                experimental_features: unsafe {
                    wgpu::ExperimentalFeatures::enabled()
                },
                ..Default::default()
            },
        ))
        .expect("创建 wgpu device 失败");

        // 2) 把同一套 wgpu 交给 Slint —— 它的渲染器就用这个 device,才能采样 bevy 产的纹理。
        slint::BackendSelector::new()
            .require_wgpu_29(slint::wgpu_29::WGPUConfiguration::Manual {
                instance: instance.clone(),
                adapter: adapter.clone(),
                device: device.clone(),
                queue: queue.clone(),
            })
            .select()
            .expect("选择 Slint 的 wgpu-29 后端失败");

        // 3) 用同一套 wgpu 建 bevy App(Manual),禁窗口/winit,无头。
        let render_creation = RenderCreation::manual(
            RenderDevice::from(device.clone()),
            RenderQueue(Arc::new(WgpuWrapper::new(queue.clone()))),
            RenderAdapterInfo(WgpuWrapper::new(adapter.get_info())),
            RenderAdapter(Arc::new(WgpuWrapper::new(adapter.clone()))),
            RenderInstance(Arc::new(WgpuWrapper::new(instance.clone()))),
        );

        let mut app = App::new();
        app.add_plugins(
            DefaultPlugins
                .set(RenderPlugin {
                    render_creation,
                    ..default()
                })
                .set(WindowPlugin {
                    // 无头:不建主窗口,Slint 才是那个有窗口的。
                    primary_window: None,
                    exit_condition: ExitCondition::DontExit,
                    ..default()
                })
                // 关掉管线化渲染:它会把渲染子 app 移到另一个线程,`get_sub_app(RenderApp)`
                // 就取不到离屏纹理了。手动驱动 + 同步取纹理,必须让渲染子 app 留在本线程。
                .disable::<bevy::render::pipelined_rendering::PipelinedRenderingPlugin>(),
        );

        // 4) 造离屏目标图 —— Rgba8Unorm 满足 Slint 的导入要求(格式 + TEXTURE_BINDING|RENDER_ATTACHMENT)。
        let image = Image::new_target_texture(
            WIDTH,
            HEIGHT,
            TextureFormat::Rgba8Unorm,
            Some(TextureFormat::Rgba8UnormSrgb),
        );
        let target = app.world_mut().resource_mut::<Assets<Image>>().add(image);

        // 5) 造网格/材质并摆好相机、光、会转的立方体。相机渲染进离屏目标图。
        spawn_scene(&mut app, &target);

        // 手动驱动模式下,首帧前要走完插件的 finish/cleanup(平时由 App::run 的 runner 负责)。
        app.finish();
        app.cleanup();

        // 转动系统。
        app.add_systems(Update, spin);

        Self {
            app,
            target,
            image: None,
            frames: 0,
        }
    }

    /// 驱动 bevy 前进一帧,返回当前离屏纹理包装成的 [`slint::Image`]。
    ///
    /// 首帧纹理还没就绪时返回空图(UI 面板据此暂不显示)。纹理一旦建出就身份稳定,
    /// 只包装一次、之后复用 —— 内容每帧由 bevy 重画,Slint 重绘时实时采样。
    pub fn render_frame(&mut self) -> slint::Image {
        self.app.update();
        self.frames += 1;

        if self.image.is_none() {
            if let Some(tex) = self.extract_texture() {
                let (w, h) = (tex.width(), tex.height());
                match slint::Image::try_from(tex) {
                    Ok(img) => {
                        log::info!(
                            "render3d: 首帧就绪(第 {} 帧),wgpu 纹理 {w}x{h} 已导入 Slint",
                            self.frames
                        );
                        self.image = Some(img);
                    }
                    Err(e) => log::error!("wgpu 纹理导入 Slint 失败: {e:?}"),
                }
            } else if self.frames == 120 {
                // 两秒还没就绪,多半是渲染子世界里没准备出 GpuImage —— 值得告警排查。
                log::warn!("render3d: 已 120 帧仍未取到离屏纹理,3D 面板不会显示");
            }
        }
        self.image.clone().unwrap_or_default()
    }

    /// 从 bevy 的渲染子世界里取出离屏目标图对应的 `wgpu::Texture`。
    fn extract_texture(&self) -> Option<SharedTexture> {
        let gpu_images = self
            .app
            .get_sub_app(RenderApp)?
            .world()
            .get_resource::<RenderAssets<GpuImage>>()?;
        let gpu_image = gpu_images.get(&self.target)?;
        // GpuImage.texture: render_resource::Texture, Deref 到 wgpu::Texture。
        Some((*gpu_image.texture).clone())
    }
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}

/// 摆放相机(渲染进离屏目标图)、平行光、会自转的立方体。
fn spawn_scene(app: &mut App, target: &Handle<Image>) {
    let world = app.world_mut();

    let cube = world
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::default());
    let material = world
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial {
            base_color: Color::srgb(0.4, 0.6, 0.9),
            ..default()
        });

    // 会自转的立方体。
    world.spawn((
        Mesh3d(cube),
        MeshMaterial3d(material),
        Transform::default(),
        Spin,
    ));

    // 平行光。
    world.spawn((
        DirectionalLight::default(),
        Transform::from_xyz(4.0, 8.0, 4.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // 相机:渲染进离屏目标图,而非屏幕。0.19 起 RenderTarget 是独立组件,不再是 Camera 的字段。
    world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.clone().into()),
        // 默认的 TonyMcMapFace 需要 tonemapping_luts feature(会拉 LUT 资源)。
        // PoC 不启那个 feature,改用无需 LUT 的 None。要更好观感时再开该 feature。
        Tonemapping::None,
        Transform::from_xyz(0.0, 1.5, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

/// 让带 [`Spin`] 的实体绕 Y 轴匀速自转。
fn spin(time: Res<Time>, mut q: Query<&mut Transform, With<Spin>>) {
    for mut t in &mut q {
        t.rotate_y(time.delta_secs());
    }
}
