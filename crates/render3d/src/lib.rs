//! 3D 桥:用 **bevy** 在**共享的** wgpu-29 device 上离屏渲染,产出一张 [`slint::Image`]
//! 交给 UI 层合成。桌面 / android / web 入口按 `bevy-3d` feature 依赖本 crate,
//! ios 永不碰它;web / ios 的**默认**构建不拉它 —— 由 `xtask boundaries` 守住这条边界。
//!
//! 架构约束(见计划 `bevy-serialized-dove`):
//! - device 由本 crate 自建(Manual),同一套 instance/adapter/device/queue
//!   既注入 Slint 的 `require_wgpu_29`,也注入 bevy 的 `RenderCreation::manual`。
//! - bevy 主线程无头运行,禁 `bevy_winit`,由 Slint 的 `Timer` 每帧驱动 `app.update()`,
//!   绝不调 `App::run()` —— 事件循环永远归 Slint。
//! - bevy 与 Slint 共享同一 wgpu 大版本(现为 29),纹理类型才是同一个,才能被 Slint 采样。
//!
//! 每帧产出**两张**图:场景本身,以及一张只含「比注释卡片更近」的片元的遮挡层
//! (见 [`spawn_occluder_camera`])。UI 侧把二者夹着卡片叠三层,卡片就被 3D 物体
//! 逐像素挡住 —— 深度正确的 UI。
//!
//! 用法(见 `apps/desktop`):先 [`Scene::new`](Scene::new) —— 它顺带配好 Slint 的 wgpu 后端,
//! 必须在建窗口**之前**调 —— 再把 `move || scene.render_frame()` 交给 `ui::run_with_renderer`。
//! web 入口(`apps/web`)用异步版 [`Scene::new_async`],其余接线相同。

use std::sync::Arc;

use bevy::prelude::*;
// BSN(next-gen 场景系统,bevy_scene feature)的 bsn! 宏、Scene/SceneList、
// World::spawn_scene 都已在 bevy::prelude 里,无需额外 use。见 rebuild_content。
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。
use bevy::camera::{
    Camera3dDepthLoadOp, ClearColorConfig, RenderTarget,
};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::platform::time::Instant;
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

mod navglass;
pub use navglass::{NavGlassPass, NavParams};

mod warp;
pub use warp::{AUDIO_BYTES, WarpPass};

/// bevy 与 slint 共享的 `wgpu::Texture` 类型别名(经 slint 的 wgpu_29 再导出,与 bevy 同一份 crate)。
pub type SharedTexture = wgpu::Texture;

/// 离屏画面尺寸。固定分辨率,Slint 侧按面板大小缩放(见计划:先不做动态 resize)。
const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;

/// 每帧耗时日志的采样窗口(帧)。约两秒一行,够看趋势又不刷屏。
const PERF_WINDOW: u32 = 120;

/// 注释卡片挂在场景里的哪个世界点。转盘中心 —— 环上的形状半圈在它前面、半圈在后面,
/// 转一圈就能看到同一个物体先挡住卡片、再转到卡片背后。
const CARD_ANCHOR: Vec3 = Vec3::ZERO;

/// 空遮挡层对应的深度清除值:近平面。反向 Z 下没有片元比近平面更近,这一层因此全空。
const EMPTY_OCCLUDER_DEPTH: f32 = 1.0;

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
        (
            self.scene_id,
            self.count,
            self.color_rgb,
            self.spacing.to_bits(),
        )
    }
}

/// 一个自持的 bevy 离屏渲染场景。
///
/// 持有 bevy `App`、离屏目标图的句柄、被拖动的立方体实体,以及一次性包装好的
/// [`slint::Image`]。整个对象只在 Slint 的主线程上被 [`render_frame`](Scene::render_frame) 驱动。
pub struct Scene {
    app: App,
    /// 共享 wgpu 的 device/queue 句柄(clone,廉价 Arc)。留着好让导航选中器的
    /// 独立 pass([`NavGlassPass`])在同一块 device 上起管线 —— 见 apps/* 的 seam。
    device: wgpu::Device,
    queue: wgpu::Queue,
    target: Handle<Image>,
    /// 转盘根实体:所有可见形状挂它下面,每帧按 yaw/pitch(+自转)设其朝向。
    /// 场景/数量/颜色/间距变化时,despawn 它的子树并按新参数用 bsn! 重建。
    root: Entity,
    /// 渲染到离屏图的相机。尺寸变化时要改它的 RenderTarget 指向新纹理。
    camera: Entity,
    /// 遮挡层的离屏目标图:同一个场景,但只留比 [`CARD_ANCHOR`] 更近的片元,其余透明。
    occluder_target: Handle<Image>,
    /// 画遮挡层的第二台相机(见 [`spawn_occluder_camera`])。
    occluder_camera: Entity,
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
    /// 遮挡层纹理包装成的 Image 及其 (宽, 高)。缓存理由同 `image`:纹理身份稳定就只包一次。
    /// 这一层不过玻璃 pass,故 key 里没有那个开关。
    occluder_image: Option<slint::Image>,
    occluder_key: Option<(u32, u32)>,
    /// 已驱动的帧数。仅用于诊断:纹理迟迟不就绪时给一次告警。
    frames: u32,
    /// 每帧耗时的累加器(毫秒):(bevy `app.update()`, 玻璃 pass)。
    /// 每 [`PERF_WINDOW`] 帧算一次均值打日志再清零 —— web 上帧率卡在 50 时,
    /// 要先知道那 20ms 花在哪一边,才谈得上优化。
    perf: (f64, f64),
}

impl Scene {
    /// 自建共享 wgpu device、配置 Slint 后端、搭好 bevy 无头渲染场景。
    ///
    /// **必须在创建任何 Slint 窗口之前调用** —— `require_wgpu_29(...).select()` 是全局的,
    /// 一旦窗口建出来就晚了。
    /// 原生平台专用:wasm 主线程不许阻塞,web 入口用 [`Scene::new_async`]。
    pub fn new() -> Self {
        bevy::tasks::block_on(Self::new_async())
    }

    /// [`Scene::new`] 的异步版:adapter/device 的 future 在原生平台首次 poll 即就绪,
    /// 只有浏览器的 WebGPU Promise 是真异步 —— 这是本方法存在的唯一原因(与 slint#11580 同思路)。
    /// wasm 上 `Backends::PRIMARY` 只含 BrowserWebGpu:浏览器没有 WebGPU 就在
    /// `request_adapter` 处 panic,不做 WebGL 降级(bevy 的渲染管线在 WebGL 下受限,不接)。
    pub async fn new_async() -> Self {
        // 1) 自建一套 wgpu。经 slint 的 wgpu_29 再导出拿到 wgpu,保证和 bevy 是同一份 crate。
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference:
                    wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            })
            .await
            .expect("找不到可用的 wgpu adapter");
        // 用 adapter 支持的全部 features/limits 建 device,确保 bevy 想要什么都有。
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
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
            })
            .await
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
        let plugins = DefaultPlugins
            .set(RenderPlugin {
                render_creation,
                // 管线编译走同步。默认的异步编译把任务丢进 bevy 的任务池,而 wasm
                // 是单线程 —— 任务迟迟不完成,渲染就一直画不出东西(纹理有、内容空)。
                // 原生上同步编译只是把首帧的卡顿提前,代价可接受,故不分平台。
                synchronous_pipeline_compilation: true,
                ..default()
            })
            .set(WindowPlugin {
                // 无头:不建主窗口,Slint 才是那个有窗口的。
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                ..default()
            });
        // 关掉管线化渲染:它会把渲染子 app 移到另一个线程,`get_sub_app(RenderApp)`
        // 就取不到离屏纹理了。手动驱动 + 同步取纹理,必须让渲染子 app 留在本线程。
        // wasm 上该模块整个被 bevy cfg 掉(天然单线程,无此插件),不禁而自禁。
        #[cfg(not(target_arch = "wasm32"))]
        let plugins = plugins.disable::<
            bevy::render::pipelined_rendering::PipelinedRenderingPlugin,
        >();
        app.add_plugins(plugins);
        // 玻璃后处理长在 bevy 的管线里,不再自己起 pass 自己提交(理由见 glass.rs)。
        app.add_plugins(glass::GlassPlugin);

        // 4) 造初始离屏目标图。UI 传来的面板尺寸会触发按需重建(动态分辨率,见 render_frame)。
        let target = make_target(&mut app, WIDTH, HEIGHT);

        // 5) 摆相机(渲染进离屏目标图)、平行光,建图元网格调色板,建一个空的转盘根。
        //    场景内容(形状)首帧按 UI 传入的 SceneParams 用 bsn! 构建(见 render_frame)。
        let camera =
            spawn_camera_and_light(&mut app, &target);
        // 6) 遮挡层:第二张目标图 + 第二台相机,合成顺序上排在卡片之后(见其文档)。
        let aspect = WIDTH as f32 / HEIGHT as f32;
        let occluder_target =
            make_target(&mut app, WIDTH, HEIGHT);
        let occluder_camera = spawn_occluder_camera(
            &mut app,
            &occluder_target,
            aspect,
        );
        let mesh_palette = build_mesh_palette(&mut app);
        let root = app
            .world_mut()
            .spawn(Transform::default())
            .id();

        // 手动驱动模式下,首帧前要走完插件的 finish/cleanup(平时由 App::run 的 runner 负责)。
        app.finish();
        app.cleanup();

        Self {
            app,
            device: device.clone(),
            queue: queue.clone(),
            target,
            root,
            camera,
            occluder_target,
            occluder_camera,
            mesh_palette,
            last_key: None,
            spin_angle: 0.0,
            size: (WIDTH, HEIGHT),
            image: None,
            image_key: None,
            occluder_image: None,
            occluder_key: None,
            perf: (0.0, 0.0),
            frames: 0,
        }
    }

    /// 共享 wgpu 的 device 句柄(clone,廉价 Arc)。供导航选中器的 [`NavGlassPass`]
    /// 在同一块 device 上起管线 —— 纹理才和 Slint 是同一份,采样得出来。
    pub fn device(&self) -> wgpu::Device {
        self.device.clone()
    }

    /// 共享 wgpu 的 queue 句柄(clone)。理由同 [`device`](Scene::device)。
    pub fn queue(&self) -> wgpu::Queue {
        self.queue.clone()
    }

    /// 按 UI 传入的 [`SceneParams`] 和面板尺寸渲染一帧,返回 **(场景, 遮挡层)** 两张
    /// 离屏纹理包装成的 [`slint::Image`]。
    ///
    /// 两张图的用法见 [`spawn_occluder_camera`]:UI 侧把它们夹着注释卡片叠三层,
    /// 卡片就被场景里更近的物体逐像素挡住。返回裸元组而不是自定义结构体 ——
    /// `slint::Image` 是 ui 与 render3d 本就共有的类型,新造一个镜像结构体只是多一份
    /// 要同步的字段(镜像的代价见 [`SceneParams`] 的注释)。
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
    ) -> (slint::Image, slint::Image) {
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
            self.app
                .world_mut()
                .get_mut::<Transform>(self.root)
        {
            transform.rotation = Quat::from_euler(
                EulerRot::YXZ,
                params.yaw + self.spin_angle,
                params.pitch,
                0.0,
            );
        }

        // 玻璃参数挂在相机上,由 bevy 的 FullscreenMaterial 在管线里消费。空矩形时摘掉
        // 组件,那一帧连全屏 pass 都不跑。必须在 update 之前设好。
        let t_glass = Instant::now();
        let glass_on = !params.glass.is_empty();
        {
            let mut cam = self
                .app
                .world_mut()
                .entity_mut(self.camera);
            if glass_on {
                cam.insert(glass::GlassMaterial::new(
                    params.glass,
                    self.size,
                ));
            } else {
                cam.remove::<glass::GlassMaterial>();
            }
        }
        self.perf.1 +=
            t_glass.elapsed().as_secs_f64() * 1000.0;

        // 遮挡层的深度门槛。用的是上一帧传播完的相机 GlobalTransform —— 相机只在 resize
        // 时移动,那一帧的门槛会差一帧,肉眼不可见,不值得为它多跑一次 transform 传播。
        let depth = self.anchor_depth();
        if let Some(mut cam3d) =
            self.app
                .world_mut()
                .get_mut::<Camera3d>(self.occluder_camera)
        {
            cam3d.depth_load_op =
                Camera3dDepthLoadOp::Clear(depth);
        }

        let t_update = Instant::now();
        self.app.update();
        self.perf.0 +=
            t_update.elapsed().as_secs_f64() * 1000.0;
        self.frames += 1;

        let Some(tex) = self.extract_texture(&self.target)
        else {
            if self.frames == 120 {
                // 两秒还没就绪,多半是渲染子世界里没准备出 GpuImage —— 值得告警排查。
                log::warn!(
                    "render3d: 已 120 帧仍未取到离屏纹理,3D 面板不会显示"
                );
            }
            return self.frame_images();
        };

        if self.frames.is_multiple_of(PERF_WINDOW) {
            let n = f64::from(PERF_WINDOW);
            log::info!(
                "render3d: 近 {PERF_WINDOW} 帧均耗时 —— app.update() {:.2}ms,玻璃 pass {:.2}ms,合计 {:.2}ms({}x{},遮挡门槛 {:.5})",
                self.perf.0 / n,
                self.perf.1 / n,
                (self.perf.0 + self.perf.1) / n,
                self.size.0,
                self.size.1,
                depth,
            );
            self.perf = (0.0, 0.0);
        }

        // 尺寸稳定、玻璃开关不变时,纹理身份稳定 → 只包装一次 Image,之后每帧由
        // bevy + 玻璃 pass 重画内容,Slint 重绘时实时采样同一张。
        let key = (tex.width(), tex.height(), glass_on);
        if self.image_key != Some(key) {
            let (w, h) = (tex.width(), tex.height());
            match slint::Image::try_from(tex) {
                Ok(img) => {
                    log::info!(
                        "render3d: 纹理就绪(第 {} 帧),{w}x{h} 已导入 Slint(玻璃 {})",
                        self.frames,
                        if glass_on {
                            "开"
                        } else {
                            "关"
                        }
                    );
                    self.image = Some(img);
                    self.image_key = Some(key);
                }
                Err(e) => log::error!(
                    "wgpu 纹理导入 Slint 失败: {e:?}"
                ),
            }
        }

        // 遮挡层同理:身份稳定就只包一次。它比场景图晚一帧就绪也无妨 —— UI 侧的
        // `occluder-3d.width > 0` 守卫会让卡片先以不被遮挡的样子出现。
        if let Some(tex) =
            self.extract_texture(&self.occluder_target)
        {
            let key = (tex.width(), tex.height());
            if self.occluder_key != Some(key) {
                match slint::Image::try_from(tex) {
                    Ok(img) => {
                        log::info!(
                            "render3d: 遮挡层就绪(第 {} 帧),{}x{} 已导入 Slint",
                            self.frames,
                            key.0,
                            key.1
                        );
                        self.occluder_image = Some(img);
                        self.occluder_key = Some(key);
                    }
                    Err(e) => log::error!(
                        "遮挡层纹理导入 Slint 失败: {e:?}"
                    ),
                }
            }
        }

        self.frame_images()
    }

    /// 当前这一帧交给 UI 的两张图:(场景, 遮挡层)。任一未就绪时给空图。
    fn frame_images(&self) -> (slint::Image, slint::Image) {
        (
            self.image.clone().unwrap_or_default(),
            self.occluder_image.clone().unwrap_or_default(),
        )
    }

    /// [`CARD_ANCHOR`] 在主相机里的 NDC 深度,供遮挡层清除深度缓冲用(见 [`occluder_depth`])。
    fn anchor_depth(&self) -> f32 {
        let camera_entity =
            self.app.world().entity(self.camera);
        let (Some(camera), Some(transform)) = (
            camera_entity.get::<Camera>(),
            camera_entity.get::<GlobalTransform>(),
        ) else {
            return EMPTY_OCCLUDER_DEPTH;
        };
        occluder_depth(
            camera.world_to_ndc(transform, CARD_ANCHOR),
        )
    }

    /// 从 bevy 的渲染子世界里取出某张离屏目标图对应的 `wgpu::Texture`。
    fn extract_texture(
        &self,
        handle: &Handle<Image>,
    ) -> Option<SharedTexture> {
        let gpu_images = self
            .app
            .get_sub_app(RenderApp)?
            .world()
            .get_resource::<RenderAssets<GpuImage>>()?;
        let gpu_image = gpu_images.get(handle)?;
        // GpuImage.texture: render_resource::Texture, Deref 到 wgpu::Texture。
        Some((*gpu_image.texture).clone())
    }

    /// 按新尺寸重建离屏目标纹理,把相机的 RenderTarget 指过去,释放旧纹理。
    ///
    /// 相机的投影长宽比由 bevy 每帧依据渲染目标尺寸自动更新,无需在此处理;但**距离**要:
    /// 透视投影固定的是垂直视野,视口一竖水平视野就跟着收窄,横排的内容会出画 ——
    /// 故按长宽比整体后撤相机(见 [`pullback`])。
    /// 重建后 `image` 置空,下一帧重新把新纹理导入 Slint。
    fn resize(&mut self, width: u32, height: u32) {
        let new_target =
            make_target(&mut self.app, width, height);
        let aspect = width as f32 / height as f32;
        self.app
            .world_mut()
            .entity_mut(self.camera)
            .insert((
                RenderTarget::Image(
                    new_target.clone().into(),
                ),
                Transform::from_translation(camera_pos(
                    aspect,
                ))
                .looking_at(Vec3::ZERO, Vec3::Y),
            ));

        // 遮挡层必须与场景图同尺寸同视角,否则两层对不上,遮挡会整体错位。
        let new_occluder =
            make_target(&mut self.app, width, height);
        self.app
            .world_mut()
            .entity_mut(self.occluder_camera)
            .insert((
                RenderTarget::Image(
                    new_occluder.clone().into(),
                ),
                Transform::from_translation(camera_pos(
                    aspect,
                ))
                .looking_at(Vec3::ZERO, Vec3::Y),
            ));

        let old =
            std::mem::replace(&mut self.target, new_target);
        let old_occluder = std::mem::replace(
            &mut self.occluder_target,
            new_occluder,
        );
        let mut images = self
            .app
            .world_mut()
            .resource_mut::<Assets<Image>>();
        images.remove(&old);
        images.remove(&old_occluder);

        self.size = (width, height);
        self.image = None;
        self.image_key = None;
        self.occluder_image = None;
        self.occluder_key = None;
    }

    /// 按新参数重建转盘内容:despawn 旧根子树,用 bsn! 声明式生成 root + 子形状。
    ///
    /// 子实体同构(都 `Mesh3d/MeshMaterial3d/Transform`,仅注入不同网格/位置),
    /// 故能收进一个 `Vec<impl Scene>` 当 `Children`。材质(基础色)与摆位随参数每次重算。
    fn rebuild_content(&mut self, params: &SceneParams) {
        // 旧转盘连同子树整体销毁(despawn 递归清子实体),换上全新的根。
        self.app
            .world_mut()
            .entity_mut(self.root)
            .despawn();

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

        // 场景重建后点一次实体数。空场景与「渲染没画出来」在画面上无法区分,
        // 这行日志把两者分开:计数为 0 说明是构建侧的问题,不必去查渲染管线。
        let meshes = self
            .app
            .world_mut()
            .query::<&Mesh3d>()
            .iter(self.app.world())
            .count();
        log::info!(
            "render3d: 场景重建完成,可渲染实体 {meshes} 个"
        );
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
            // 首帧的目标是 WIDTH×HEIGHT;真实面板尺寸一到,resize 会按其长宽比重设位置与投影。
            Transform::from_translation(camera_pos(
                WIDTH as f32 / HEIGHT as f32,
            ))
            .looking_at(Vec3::ZERO, Vec3::Y),
        ))
        .id()
}

/// 第二台相机:与主相机同位、同投影、同色调映射,渲染进**遮挡层**目标图。
///
/// 这一台是「深度正确的 UI」的全部机关。UI 侧把画面叠成三层 —— 场景、Slint 卡片、
/// 遮挡层 —— 于是卡片被场景里更近的物体逐像素挡住,而 Slint 只做了寻常的 alpha 合成:
/// 它不需要知道深度,UI 也不需要先渲进纹理。
///
/// 与主相机只差两处,合起来就是那个效果:
/// - 清除色透明:没画到的地方 alpha 为 0,合成时露出下面的卡片;
/// - 深度缓冲不清到远平面,而是清到卡片锚点的深度(每帧由 `render_frame` 填,
///   见 [`occluder_depth`]),于是只有**比卡片更近**的片元能过 `GreaterEqual`。
///
/// 逐片元是关键:一个横跨锚点平面的立方体会被平面切开,而不是整体跳到卡片前面或后面。
/// 这是 CPU 侧按物体排序做不到的,也正是「合成器把 UI 整层贴在 canvas 上」的方案
/// 在原理上做不到的那件事。
///
/// ponytail: 代价是几何被提交两遍。8~64 个形状时可忽略;真要省,可在卡片隐藏时
/// 把这台相机 `is_active` 关掉,或改成采样深度纹理的一个全屏 pass(那需要自建
/// 渲染图节点与 WGSL,现在不值)。
///
/// `order` 排在主相机之后,只为让两次渲染先后确定;二者目标不同,并无依赖。
fn spawn_occluder_camera(
    app: &mut App,
    target: &Handle<Image>,
    aspect: f32,
) -> Entity {
    app.world_mut()
        .spawn((
            Camera3d {
                // 首帧的占位值,真值每帧由 render_frame 填。
                depth_load_op: Camera3dDepthLoadOp::Clear(
                    EMPTY_OCCLUDER_DEPTH,
                ),
                ..default()
            },
            Camera {
                order: 1,
                clear_color: ClearColorConfig::Custom(
                    Color::NONE,
                ),
                ..default()
            },
            RenderTarget::Image(target.clone().into()),
            // 必须与主相机一致:同一个物体在两层里出现时颜色要逐像素相同,
            // 否则被切开的那半会显出另一种色调,穿帮。
            Tonemapping::None,
            Transform::from_translation(camera_pos(aspect))
                .looking_at(Vec3::ZERO, Vec3::Y),
        ))
        .id()
}

/// 遮挡层的深度清除值:锚点的 NDC 深度。
///
/// bevy 用反向 Z(1 是近平面,0 是远平面),深度测试是 `GreaterEqual`。把深度缓冲
/// 预先清到这个值,只有比锚点更近的片元能通过测试画进遮挡层。
///
/// 锚点跑到相机背后、或投影退化出非有限值时退回 [`EMPTY_OCCLUDER_DEPTH`]:遮挡层为空,
/// 卡片完整可见。宁可少一个效果,也不能把整幅场景糊在卡片上 —— 后者是刺眼的错画面。
/// wgpu 另有硬性要求:深度清除值必须落在 [0, 1],越界会被校验层拒掉。
fn occluder_depth(anchor_ndc: Option<Vec3>) -> f32 {
    match anchor_ndc {
        Some(ndc)
            if ndc.z.is_finite()
                && (0.0..=1.0).contains(&ndc.z) =>
        {
            ndc.z
        }
        _ => EMPTY_OCCLUDER_DEPTH,
    }
}

/// 相机在参考长宽比下的位置。宽视口恒用这个位置 —— 改动前的观感基线。
const BASE_CAMERA_POS: Vec3 = Vec3::new(0.0, 1.5, 8.0);
/// 参考长宽比。视口比它更"竖",相机就得后撤。
const REFERENCE_ASPECT: f32 = 1.0;
/// 后撤倍数上限。窗口被拖成 1px 宽时长宽比趋近 0,不封顶相机会飞到无穷远。
/// 4 倍够手机竖屏(长宽比 ~0.26,需要 3.8 倍)完整入画。
const MAX_PULLBACK: f32 = 4.0;

/// 相机后撤倍数,随视口长宽比反比放大。
///
/// 透视投影固定的是**垂直**视野,水平视野 = 垂直视野 × 长宽比 —— 视口一竖,横向排开的
/// 形状画廊就出画。距离与长宽比成反比放大即可把它拉回来。宽视口(≥ 参考比)返回 1,
/// 桌面观感与改动前逐像素一致。
///
/// 用**倍数**而不是直接给 z:相机位置整体乘这个倍数,视线方向不变,俯视角就保住了。
/// 只退 z 的话相机会越退越水平,转盘被看成侧视的一条扁线 —— 试过,很难看。
///
/// 退化输入(0 / 负 / NaN,来自 1px 窗口或尚未测量的首帧)返回 1,不放大。
fn pullback(aspect: f32) -> f32 {
    if !aspect.is_finite() || aspect <= 0.0 {
        return 1.0;
    }
    (REFERENCE_ASPECT / aspect).clamp(1.0, MAX_PULLBACK)
}

/// 给定视口长宽比,相机该待的位置。
fn camera_pos(aspect: f32) -> Vec3 {
    BASE_CAMERA_POS * pullback(aspect)
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
                (
                    mesh,
                    Vec3::new(
                        radius * a.cos(),
                        0.0,
                        radius * a.sin(),
                    ),
                )
            })
            .collect()
    } else {
        let dim = (count as f32).sqrt().ceil() as usize;
        let offset =
            (dim as f32 - 1.0) * params.spacing / 2.0;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 宽视口(长宽比 ≥ 参考比)不后撤 —— 桌面上的观感必须与改动前逐像素一致。
    #[test]
    fn wide_viewport_keeps_base_distance() {
        assert_eq!(pullback(REFERENCE_ASPECT), 1.0);
        assert_eq!(pullback(16.0 / 9.0), 1.0);
        assert_eq!(pullback(100.0), 1.0);
        assert_eq!(camera_pos(16.0 / 9.0), BASE_CAMERA_POS);
    }

    /// 竖视口按长宽比等比后撤 —— 水平视野随长宽比线性收窄,故距离须反比放大,
    /// 横向排开的形状画廊才不会出画。整体缩放位置向量,俯视角不变。
    #[test]
    fn portrait_viewport_pulls_camera_back_proportionally()
    {
        // 长宽比减半 → 后撤一倍。
        assert_eq!(pullback(0.5), 2.0);
        assert_eq!(camera_pos(0.5), BASE_CAMERA_POS * 2.0);
        // 手机竖屏的 3D 面板约 0.8:轻微后撤。
        assert!(
            (pullback(0.8) - 1.0 / 0.8).abs() < 1e-4,
            "0.8 处应精确等于 参考比 / aspect"
        );
        // 视线方向(俯视角)必须与基线一致 —— 只退 z 会把转盘看成侧视的一条扁线。
        let far = camera_pos(0.3).normalize();
        let base = BASE_CAMERA_POS.normalize();
        assert!(
            (far - base).length() < 1e-5,
            "后撤后视线方向变了:{far:?} vs {base:?}"
        );
    }

    /// 参考比处连续 —— 断点两侧不能跳变,否则拖动窗口过临界点时 3D 画面会"咔"一下。
    #[test]
    fn distance_is_continuous_at_reference_aspect() {
        let below = pullback(REFERENCE_ASPECT - 1e-3);
        let above = pullback(REFERENCE_ASPECT + 1e-3);
        assert!(
            (below - above).abs() < 1e-2,
            "参考比两侧应连续,实测 {below} vs {above}"
        );
    }

    /// 锚点在视锥内:深度门槛就是它自己的 NDC z,遮挡层据此只留更近的片元。
    #[test]
    fn anchor_in_frustum_becomes_the_depth_threshold() {
        assert_eq!(
            occluder_depth(Some(Vec3::new(0.0, 0.0, 0.42))),
            0.42
        );
        // 两个端点也是合法值:近平面(全空)与远平面(全遮挡)。
        assert_eq!(occluder_depth(Some(Vec3::ZERO)), 0.0);
        assert_eq!(
            occluder_depth(Some(Vec3::new(0.0, 0.0, 1.0))),
            1.0
        );
    }

    /// 边界:锚点投影不出来(在相机背后)或落在 [0,1] 之外时,遮挡层必须为空。
    ///
    /// 这条是防错画面的,不是防崩溃:清除值越界会被 wgpu 校验层拒掉,而"退回整幅场景
    /// 糊住卡片"比"少一个遮挡效果"难看得多。
    #[test]
    fn anchor_outside_the_frustum_empties_the_occluder() {
        assert_eq!(
            occluder_depth(None),
            EMPTY_OCCLUDER_DEPTH
        );
        for z in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
            assert_eq!(
                occluder_depth(Some(Vec3::new(
                    0.0, 0.0, z
                ))),
                EMPTY_OCCLUDER_DEPTH,
                "NDC z = {z} 应该退回空遮挡层"
            );
        }
    }

    /// 退化输入不产生疯狂结果:长宽比为 0 / 负数 / NaN(窗口被拖到 1px、首帧未测量)时
    /// 后撤倍数必须仍是有限正值,且封顶 —— 相机不能飞到无穷远。
    #[test]
    fn degenerate_aspect_is_clamped_to_finite_distance() {
        for aspect in
            [0.0, -1.0, f32::NAN, f32::INFINITY, 1e-9]
        {
            let k = pullback(aspect);
            assert!(
                k.is_finite()
                    && (1.0..=MAX_PULLBACK).contains(&k),
                "aspect {aspect} 得到不合理的后撤倍数 {k}"
            );
        }
    }
}
