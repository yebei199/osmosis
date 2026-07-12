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
// BSN(next-gen 场景系统,bevy_scene feature)的 bsn! 宏、Scene/SceneList、
// World::spawn_scene 都已在 bevy::prelude 里,无需额外 use。见 rebuild_content。
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。
use bevy::camera::RenderTarget;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::render::RenderApp;
use bevy::render::RenderPlugin;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::TextureFormat;
use bevy::render::renderer::{
    RenderAdapter, RenderAdapterInfo, RenderDevice,
    RenderInstance, RenderQueue, WgpuWrapper,
};
use bevy::render::settings::RenderCreation;
use bevy::render::texture::GpuImage;
use bevy::window::{ExitCondition, WindowPlugin};
use slint::wgpu_29::wgpu;

mod glass;
pub use glass::GlassRect;

/// bevy 与 slint 共享的 `wgpu::Texture` 类型别名(经 slint 的 wgpu_29 再导出,与 bevy 同一份 crate)。
pub type SharedTexture = wgpu::Texture;

/// 离屏画面尺寸。固定分辨率,Slint 侧按面板大小缩放(见计划:先不做动态 resize)。
const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;

/// 一帧的场景控制量:由 UI 侧组装,跨进程内边界传给渲染器。
///
/// ponytail: 本结构与 `ui::SceneControls` 字段镜像。ui 与 render3d 刻意互不依赖
/// (见 apps/android 注释:app 才是接 seam 的组合根),没有二者都依赖的下层 crate 可放它,
/// 故各留一份、由 apps/* 在 seam 处平凡字段拷贝。若两者开始漂移或出现第三个消费者,
/// 再抽一个共享叶子 crate。全 POD,不含 bevy 类型,`color_rgb` 在 render3d 内转 `Color`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneParams {
    /// 0 = 形状画廊(环形),1 = 实体阵列(网格)。
    pub scene_id: i32,
    /// 转盘朝向(弧度)。
    pub yaw: f32,
    pub pitch: f32,
    /// 实体数。
    pub count: u32,
    /// 基础色 0xRRGGBB。
    pub color_rgb: u32,
    /// 自转角速度(弧度/帧),叠加到 yaw 上。
    pub spin_speed: f32,
    /// 间距/缩放因子。
    pub spacing: f32,
    /// 要做成液态玻璃的那块区域(热调工具条),物理像素。宽高为 0 表示这一帧不做玻璃。
    /// 几何量由 UI 侧给出,不在这里重复 .slint 里的留白常量(见 [`GlassRect`])。
    pub glass: GlassRect,
}

impl SceneParams {
    /// 决定是否需要重建场景内容的关键字段(yaw/pitch/自转不触发重建)。
    fn content_key(&self) -> (i32, u32, u32, u32) {
        (self.scene_id, self.count, self.color_rgb, self.spacing.to_bits())
    }
}

/// 一个自持的 bevy 离屏渲染场景。
///
/// 持有 bevy `App`、离屏目标图的句柄、被拖动的立方体实体,以及一次性包装好的
/// [`slint::Image`]。整个对象只在 Slint 的主线程上被 [`render_frame`](Scene::render_frame) 驱动。
pub struct Scene {
    app: App,
    target: Handle<Image>,
    /// 转盘根实体:所有可见形状挂它下面,每帧按 yaw/pitch(+自转)设其朝向。
    /// 场景/数量/颜色/间距变化时,despawn 它的子树并按新参数用 bsn! 重建。
    root: Entity,
    /// 渲染到离屏图的相机。尺寸变化时要改它的 RenderTarget 指向新纹理。
    camera: Entity,
    /// 六种内置图元的网格句柄,建一次复用。索引 0(Cuboid)兼作阵列场景的方块。
    mesh_palette: Vec<Handle<Mesh>>,
    /// 上一帧的场景参数;`content_key` 变化才重建内容。首帧为 `None`,必重建。
    last_key: Option<(i32, u32, u32, u32)>,
    /// 累积自转角(弧度),每帧加 `spin_speed`,叠加到 yaw。
    spin_angle: f32,
    /// 当前离屏纹理尺寸。UI 传入的面板尺寸与它不同就重建纹理(动态分辨率)。
    size: (u32, u32),
    /// 首帧(或重建后)渲染出纹理才 `Some`。为空时返回空图,UI 的 `scene-3d.width > 0` 守卫据此不显示。
    image: Option<slint::Image>,
    /// `image` 里那张对应的 (宽, 高, 是否走了玻璃 pass)。三者任一变化都得重新导入 ——
    /// 玻璃开关一翻,交给 Slint 的就换成了另一张纹理。
    image_key: Option<(u32, u32, bool)>,
    /// 已驱动的帧数。仅用于诊断:纹理迟迟不就绪时给一次告警。
    frames: u32,
    /// 液态玻璃后处理。持有它自己的管线与输出纹理。
    glass: glass::GlassPass,
    /// 共享的 device / queue,玻璃 pass 每帧要用。与 Slint、bevy 是同一套。
    device: wgpu::Device,
    queue: wgpu::Queue,
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
                // 请求 adapter 支持的特性,但**摘掉 MAPPABLE_PRIMARY_BUFFERS** ——
                // wgpu 自己警告它在独显上是「massive performance footgun」:强制缓冲区
                // 走可映射的慢速内存,拖垮渲染吞吐(实测桌面因此卡到 12fps)。bevy 用不到它。
                required_features: adapter.features()
                    .difference(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS),
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
            .require_wgpu_29(
                slint::wgpu_29::WGPUConfiguration::Manual {
                    instance: instance.clone(),
                    adapter: adapter.clone(),
                    device: device.clone(),
                    queue: queue.clone(),
                },
            )
            .select()
            .expect("选择 Slint 的 wgpu-29 后端失败");

        // 3) 用同一套 wgpu 建 bevy App(Manual),禁窗口/winit,无头。
        let render_creation = RenderCreation::manual(
            RenderDevice::from(device.clone()),
            RenderQueue(Arc::new(WgpuWrapper::new(
                queue.clone(),
            ))),
            RenderAdapterInfo(WgpuWrapper::new(
                adapter.get_info(),
            )),
            RenderAdapter(Arc::new(WgpuWrapper::new(
                adapter.clone(),
            ))),
            RenderInstance(Arc::new(WgpuWrapper::new(
                instance.clone(),
            ))),
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

        // 4) 造初始离屏目标图。UI 传来的面板尺寸会触发按需重建(动态分辨率,见 render_frame)。
        let target = make_target(&mut app, WIDTH, HEIGHT);

        // 5) 摆相机(渲染进离屏目标图)、平行光,建图元网格调色板,建一个空的转盘根。
        //    场景内容(形状)首帧按 UI 传入的 SceneParams 用 bsn! 构建(见 render_frame)。
        let camera = spawn_camera_and_light(&mut app, &target);
        let mesh_palette = build_mesh_palette(&mut app);
        let root = app.world_mut().spawn(Transform::default()).id();

        // 手动驱动模式下,首帧前要走完插件的 finish/cleanup(平时由 App::run 的 runner 负责)。
        app.finish();
        app.cleanup();

        Self {
            app,
            target,
            root,
            camera,
            mesh_palette,
            last_key: None,
            spin_angle: 0.0,
            size: (WIDTH, HEIGHT),
            image: None,
            image_key: None,
            frames: 0,
            glass: glass::GlassPass::new(&device),
            device,
            queue,
        }
    }

    /// 按 UI 传入的 [`SceneParams`] 和面板尺寸渲染一帧,返回离屏纹理包装成的 [`slint::Image`]。
    ///
    /// 内容相关字段(场景/数量/颜色/间距)变化时先用 bsn! 重建场景内容;朝向字段
    /// (yaw/pitch/自转)只更新转盘根的 Transform。`width` / `height` 为面板物理像素尺寸,
    /// 与当前纹理不同就按需重建纹理(动态分辨率),0 尺寸忽略。先设好状态再 update。
    ///
    /// 纹理未就绪(首帧或刚重建)时返回空图。尺寸不变时纹理身份稳定,只包装一次、
    /// 之后复用 —— 内容每帧由 bevy 重画,Slint 重绘时实时采样。
    pub fn render_frame(
        &mut self,
        params: &SceneParams,
        width: u32,
        height: u32,
    ) -> slint::Image {
        if width > 0
            && height > 0
            && (width, height) != self.size
        {
            self.resize(width, height);
        }

        // 场景/数量/颜色/间距变了才重建内容(despawn 子树 + bsn! 重造);朝向类参数不触发。
        let key = params.content_key();
        if self.last_key != Some(key) {
            self.rebuild_content(params);
            self.last_key = Some(key);
        }

        // 自转累积,叠加到拖动的 yaw 上,整群当转盘转。
        self.spin_angle += params.spin_speed;
        if let Some(mut transform) =
            self.app.world_mut().get_mut::<Transform>(self.root)
        {
            transform.rotation = Quat::from_euler(
                EulerRot::YXZ,
                params.yaw + self.spin_angle,
                params.pitch,
                0.0,
            );
        }

        self.app.update();
        self.frames += 1;

        let Some(tex) = self.extract_texture() else {
            if self.frames == 120 {
                // 两秒还没就绪,多半是渲染子世界里没准备出 GpuImage —— 值得告警排查。
                log::warn!(
                    "render3d: 已 120 帧仍未取到离屏纹理,3D 面板不会显示"
                );
            }
            return self.image.clone().unwrap_or_default();
        };

        // 玻璃 pass:在 bevy 那张画面上,把工具条那块区域模糊+折射掉,产出另一张纹理。
        // 这一步是 Slint 自己做不到的(它没有 backdrop blur),见 glass.rs 的模块注释。
        let glass_on = !params.glass.is_empty();
        let composed = self.glass.run(
            &self.device,
            &self.queue,
            &tex,
            params.glass,
        );

        // 尺寸稳定、玻璃开关不变时,纹理身份稳定 → 只包装一次 Image,之后每帧由
        // bevy + 玻璃 pass 重画内容,Slint 重绘时实时采样同一张。
        let key = (composed.width(), composed.height(), glass_on);
        if self.image_key != Some(key) {
            let (w, h) = (composed.width(), composed.height());
            match slint::Image::try_from(composed) {
                Ok(img) => {
                    log::info!(
                        "render3d: 纹理就绪(第 {} 帧),{w}x{h} 已导入 Slint(玻璃 {})",
                        self.frames,
                        if glass_on { "开" } else { "关" }
                    );
                    self.image = Some(img);
                    self.image_key = Some(key);
                }
                Err(e) => log::error!(
                    "wgpu 纹理导入 Slint 失败: {e:?}"
                ),
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

    /// 按新尺寸重建离屏目标纹理,把相机的 RenderTarget 指过去,释放旧纹理。
    ///
    /// 相机的投影长宽比由 bevy 每帧依据渲染目标尺寸自动更新,无需在此处理。
    /// 重建后 `image` 置空,下一帧重新把新纹理导入 Slint。
    fn resize(&mut self, width: u32, height: u32) {
        let new_target =
            make_target(&mut self.app, width, height);
        self.app
            .world_mut()
            .entity_mut(self.camera)
            .insert(RenderTarget::Image(
                new_target.clone().into(),
            ));

        let old =
            std::mem::replace(&mut self.target, new_target);
        self.app
            .world_mut()
            .resource_mut::<Assets<Image>>()
            .remove(&old);

        self.size = (width, height);
        self.image = None;
        self.image_key = None;
    }

    /// 按新参数重建转盘内容:despawn 旧根子树,用 bsn! 声明式生成 root + 子形状。
    ///
    /// 子实体同构(都 `Mesh3d/MeshMaterial3d/Transform`,仅注入不同网格/位置),
    /// 故能收进一个 `Vec<impl Scene>` 当 `Children`。材质(基础色)与摆位随参数每次重算。
    fn rebuild_content(&mut self, params: &SceneParams) {
        // 旧转盘连同子树整体销毁(despawn 递归清子实体),换上全新的根。
        self.app.world_mut().entity_mut(self.root).despawn();

        let color = Color::srgb_u8(
            (params.color_rgb >> 16) as u8,
            (params.color_rgb >> 8) as u8,
            params.color_rgb as u8,
        );
        let material = self
            .app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                base_color: color,
                ..default()
            });

        // 每个形状的 (网格句柄, 位置)。画廊环形铺开、阵列网格铺开(见 compute_placements)。
        let placements =
            compute_placements(params, &self.mesh_palette);
        let kids: Vec<_> = placements
            .into_iter()
            .map(|(mesh, translation)| {
                let mat = material.clone();
                bsn! {
                    Mesh3d({mesh})
                    MeshMaterial3d::<StandardMaterial>({mat})
                    Transform { translation: {translation} }
                }
            })
            .collect();

        self.root = self
            .app
            .world_mut()
            .spawn_scene(bsn! {
                // Visibility 必配:Mesh 子实体带 Visibility(必需组件),根缺它则可见性
                // 传播每帧告警 B0004。Transform 的传播则靠 0.19 的必需组件自动补 GlobalTransform。
                Transform
                Visibility::Visible
                Children [ {kids} ]
            })
            .expect("spawn_scene 无 asset 依赖,不应失败")
            .id();
    }
}

/// 造一张 bevy 离屏渲染目标图(Rgba8Unorm,满足 Slint 导入要求:格式 +
/// TEXTURE_BINDING|RENDER_ATTACHMENT),返回其资源句柄。
fn make_target(
    app: &mut App,
    width: u32,
    height: u32,
) -> Handle<Image> {
    let image = Image::new_target_texture(
        width,
        height,
        TextureFormat::Rgba8Unorm,
        Some(TextureFormat::Rgba8UnormSrgb),
    );
    app.world_mut()
        .resource_mut::<Assets<Image>>()
        .add(image)
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}

/// 摆放相机(渲染进离屏目标图)与平行光,二者全程不变;返回相机实体供尺寸变化时改 RenderTarget。
fn spawn_camera_and_light(
    app: &mut App,
    target: &Handle<Image>,
) -> Entity {
    let world = app.world_mut();

    // 平行光。
    world.spawn((
        DirectionalLight::default(),
        Transform::from_xyz(4.0, 8.0, 4.0)
            .looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // 相机:渲染进离屏目标图,而非屏幕。0.19 起 RenderTarget 是独立组件,不再是 Camera 的字段。
    world
        .spawn((
            Camera3d::default(),
            Camera::default(),
            RenderTarget::Image(target.clone().into()),
            // 默认的 TonyMcMapFace 需要 tonemapping_luts feature(会拉 LUT 资源)。
            // PoC 不启那个 feature,改用无需 LUT 的 None。要更好观感时再开该 feature。
            Tonemapping::None,
            Transform::from_xyz(0.0, 1.5, 8.0)
                .looking_at(Vec3::ZERO, Vec3::Y),
        ))
        .id()
}

/// 建六种内置图元的网格句柄,建一次复用。索引 0 是 Cuboid,兼作阵列场景的方块。
fn build_mesh_palette(app: &mut App) -> Vec<Handle<Mesh>> {
    let mut meshes =
        app.world_mut().resource_mut::<Assets<Mesh>>();
    vec![
        meshes.add(Cuboid::default()),
        meshes.add(Sphere::default()),
        meshes.add(Torus::default()),
        meshes.add(Capsule3d::default()),
        meshes.add(Cylinder::default()),
        meshes.add(Cone::default()),
    ]
}

/// 按场景种类算出每个形状的 (网格句柄, 位置)。
///
/// - 画廊(scene_id==0):`count` 个形状沿 XZ 平面环形均布,网格在调色板里循环取,
///   环半径随 `spacing` 放大 —— 逛一圈能看到不同图元。
/// - 阵列(其它):`count` 个 Cuboid 排成近正方网格(边长 = ⌈√count⌉),`spacing` 是格距,
///   整体居中于原点。
fn compute_placements(
    params: &SceneParams,
    palette: &[Handle<Mesh>],
) -> Vec<(Handle<Mesh>, Vec3)> {
    let count = params.count.max(1) as usize;
    if params.scene_id == 0 {
        let radius = params.spacing * 1.5;
        (0..count)
            .map(|i| {
                let a = std::f32::consts::TAU * i as f32
                    / count as f32;
                let mesh =
                    palette[i % palette.len()].clone();
                (mesh, Vec3::new(radius * a.cos(), 0.0, radius * a.sin()))
            })
            .collect()
    } else {
        let dim = (count as f32).sqrt().ceil() as usize;
        let offset = (dim as f32 - 1.0) * params.spacing / 2.0;
        (0..count)
            .map(|i| {
                let (col, row) = (i % dim, i / dim);
                let pos = Vec3::new(
                    col as f32 * params.spacing - offset,
                    0.0,
                    row as f32 * params.spacing - offset,
                );
                (palette[0].clone(), pos)
            })
            .collect()
    }
}
