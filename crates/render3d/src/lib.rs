//! 3D 桥:用 **bevy** 在**共享的** wgpu-29 device 上离屏渲染,产出一张 [`slint::Image`]
//! 交给 UI 层合成。桌面 / android 入口硬依赖本 crate;web / ios 永不碰它 ——
//! 由 `xtask boundaries` 守住这条边界。
//!
//! 架构约束(见计划 `bevy-serialized-dove`):
//! - device 由本 crate 自建(Manual),同一套 instance/adapter/device/queue
//!   既注入 Slint 的 `require_wgpu_29`,也注入 bevy 的 `RenderCreation::manual`。
//! - bevy 主线程无头运行,禁 `bevy_winit`,由 Slint 的 `Timer` 每帧驱动 `app.update()`,
//!   绝不调 `App::run()` —— 事件循环永远归 Slint。
//! - bevy 与 Slint 共享同一 wgpu 大版本(现为 29),纹理类型才是同一个,才能被 Slint 采样。
//!
//! 每帧产出**两张**图:粒子场本身,以及一张只含「比封面卡更近」的片元的遮挡层
//! (见 [`spawn_occluder_camera`])。UI 侧把二者夹着卡片叠三层,卡片就被粒子
//! 逐像素挡住 —— 深度正确的 UI。
//!
//! 用法(见 `apps/desktop`):先 [`Scene::new`](Scene::new) —— 它顺带配好 Slint 的 wgpu 后端,
//! 必须在建窗口**之前**调 —— 再把 `move || scene.render_viz_frame()` 交给
//! `ui::run_with_renderers`。

use std::sync::Arc;

use bevy::prelude::*;
// BSN(next-gen 场景系统,bevy_scene feature)的 bsn! 宏、Scene/SceneList、
// World::spawn_scene 都已在 bevy::prelude 里,无需额外 use。见 rebuild_content。
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。
use bevy::asset::RenderAssetUsages;
use bevy::camera::{
    Camera3dDepthLoadOp, ClearColorConfig, RenderTarget,
};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::platform::time::Instant;
use bevy::render::RenderApp;
use bevy::render::RenderPlugin;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat,
};
use bevy::render::renderer::{
    RenderAdapter, RenderAdapterInfo, RenderDevice,
    RenderInstance, RenderQueue, WgpuWrapper,
};
use bevy::render::settings::RenderCreation;
use bevy::render::texture::GpuImage;
use bevy::window::{ExitCondition, WindowPlugin};
use slint::wgpu_29::wgpu;

mod navglass;
pub use navglass::{NavGlassPass, NavParams};

mod warp;
pub use warp::{AUDIO_BYTES, WarpPass};

mod cloud;

/// bevy 与 slint 共享的 `wgpu::Texture` 类型别名(经 slint 的 wgpu_29 再导出,与 bevy 同一份 crate)。
pub type SharedTexture = wgpu::Texture;

/// 一帧里视觉区的指针状态,POD。镜像 `ui::VizPointer`,apps/* 在 seam 处平凡拷过来。
///
/// 位置归一到 0..1(左上原点)。`active` 为假表示指针不在视觉区里,这一帧既不起
/// 涟漪也不拖动。
#[derive(Clone, Copy, Debug, Default)]
pub struct Pointer {
    pub x: f32,
    pub y: f32,
    pub down: bool,
    pub active: bool,
}

/// 驱动一帧播放页视觉要的全部输入,POD。镜像 `ui::VizControls` 加上视口尺寸。
///
/// 打包而不是摊成一串参数:这几样每加一件,`render_viz_frame` 的签名就长一截,
/// 调用处也看不出哪个位置是谁。apps/* 在 seam 处把 ui 那份平凡拷成这一份。
#[derive(Clone, Copy, Debug, Default)]
pub struct VizFrame<'a> {
    /// 播放页时钟,秒。门关即冻结。
    pub time: f32,
    /// `spectrum` 布局的载荷,频谱行在前。只用前 512 字节拆频段。
    pub audio: &'a [u8],
    /// **换歌解出新封面的那一帧**才有值:(宽, 高, RGBA8)。
    pub cover: Option<(u32, u32, &'a [u8])>,
    /// 视觉区里的指针。
    pub pointer: Pointer,
    /// 视觉预设的编号,越界回默认档。
    pub preset: i32,
    /// 窗口的物理像素尺寸。与当前纹理不同就按需重建(动态分辨率),0 尺寸忽略。
    pub width: u32,
    pub height: u32,
}

/// 离屏画面尺寸。固定分辨率,Slint 侧按面板大小缩放(见计划:先不做动态 resize)。
const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;

/// 每帧耗时日志的采样窗口(帧)。约两秒一行,够看趋势又不刷屏。
const PERF_WINDOW: u32 = 120;

/// 封面卡挂在场景里的哪个世界点。粒子场中心 —— 粒子半数在它前面、半数在后面,
/// 于是同一颗粒子飘过时会先挡住卡片、再转到卡片背后。
const CARD_ANCHOR: Vec3 = Vec3::ZERO;

/// 空遮挡层对应的深度清除值:近平面。反向 Z 下没有片元比近平面更近,这一层因此全空。
const EMPTY_OCCLUDER_DEPTH: f32 = 1.0;

/// 一个自持的 bevy 离屏渲染场景。
///
/// 持有 bevy `App`、离屏目标图的句柄、粒子实体,以及一次性包装好的
/// [`slint::Image`]。整个对象只在 Slint 的主线程上被
/// [`render_viz_frame`](Scene::render_viz_frame) 驱动。
pub struct Scene {
    app: App,
    /// 共享 wgpu 的 device/queue 句柄(clone,廉价 Arc)。留着好让导航选中器的
    /// 独立 pass([`NavGlassPass`])在同一块 device 上起管线 —— 见 apps/* 的 seam。
    device: wgpu::Device,
    queue: wgpu::Queue,
    target: Handle<Image>,
    /// 点云实体。首帧 despawn 占位实体后重建。
    root: Entity,
    /// 点云材质的句柄:每帧改它的 uniform(时间、三段电平),几何一动不动。
    cloud_material: Option<Handle<cloud::CloudMaterial>>,
    /// 换歌过渡(颜色渐变 + burst)。按播放页时钟推进。
    transition: cloud::TrackTransition,
    /// 指针涟漪表。
    ripples: cloud::Ripples,
    /// 拖动带来的点云自转与松手后的惯性。
    spin: cloud::Spin,
    /// 上一帧的指针状态,用来算这一帧拖了多少、该不该起一路涟漪。
    last_pointer: Option<(f32, f32)>,
    /// 上一帧的播放页时钟,用来算过渡要推进多少。门关着时钟不走,过渡跟着定格。
    last_time: Option<f32>,
    /// 渲染到离屏图的相机。尺寸变化时要改它的 RenderTarget 指向新纹理。
    camera: Entity,
    /// 遮挡层的离屏目标图:同一个场景,但只留比 [`CARD_ANCHOR`] 更近的片元,其余透明。
    occluder_target: Handle<Image>,
    /// 画遮挡层的第二台相机(见 [`spawn_occluder_camera`])。
    occluder_camera: Entity,
    /// 点云是否已经建好。首帧为假,建一次之后几何固定,只有 uniform 每帧在换。
    cloud_built: bool,
    /// 当前离屏纹理尺寸。UI 传入的面板尺寸与它不同就重建纹理(动态分辨率)。
    size: (u32, u32),
    /// 首帧(或重建后)渲染出纹理才 `Some`。为空时返回空图,UI 的 `viz-scene.width > 0` 守卫据此不显示。
    image: Option<slint::Image>,
    /// `image` 里那张对应的 (宽, 高)。尺寸一变纹理就换了身份,得重新导入。
    image_key: Option<(u32, u32)>,
    /// 遮挡层纹理包装成的 Image 及其 (宽, 高)。缓存理由同 `image`:纹理身份稳定就只包一次。
    /// 这一层不过玻璃 pass,故 key 里没有那个开关。
    occluder_image: Option<slint::Image>,
    occluder_key: Option<(u32, u32)>,
    /// 已驱动的帧数。仅用于诊断:纹理迟迟不就绪时给一次告警。
    frames: u32,
    /// bevy `app.update()` 的耗时累加器(毫秒)。每 [`PERF_WINDOW`] 帧算一次均值打日志
    /// 再清零 —— 帧率掉下来时,要先知道是不是 bevy 这一段吃掉的。
    perf: f64,
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
        // 点云的自定义材质,以及随二进制内嵌的着色器 —— 应用无头运行,
        // 没有 `assets/` 目录可以从磁盘加载。
        bevy::asset::embedded_asset!(app, "cloud.wgsl");
        app.add_plugins(MaterialPlugin::<
            cloud::CloudMaterial,
        >::default());

        // 4) 造初始离屏目标图。UI 传来的窗口尺寸会触发按需重建(动态分辨率,见 render_viz_frame)。
        let target = make_target(&mut app, WIDTH, HEIGHT);

        // 5) 摆相机(渲染进离屏目标图),建一个空的占位实体。
        //    点云首帧由 rebuild_viz_content 建出来。
        let camera = spawn_camera(&mut app, &target);
        // 6) 遮挡层:第二张目标图 + 第二台相机,合成顺序上排在卡片之后(见其文档)。
        let occluder_target =
            make_target(&mut app, WIDTH, HEIGHT);
        let occluder_camera = spawn_occluder_camera(
            &mut app,
            &occluder_target,
        );
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
            cloud_material: None,
            transition: cloud::TrackTransition::default(),
            ripples: cloud::Ripples::default(),
            spin: cloud::Spin::default(),
            last_pointer: None,
            last_time: None,
            camera,
            occluder_target,
            occluder_camera,
            cloud_built: false,
            size: (WIDTH, HEIGHT),
            image: None,
            image_key: None,
            occluder_image: None,
            occluder_key: None,
            perf: 0.0,
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

    /// 播放页粒子场的一帧:本 crate 唯一的驱动入口。
    ///
    /// `time` 是播放页时钟(秒,门关即冻结,见 crates/ui);`audio` 是
    /// `spectrum` 布局的载荷(频谱行在前),只用前 512 字节拆频段;
    /// 粒子位置与缩放每帧由 [`particles::particle_pose`] 算好直写 Transform。
    ///
    /// 返回 **(场景, 遮挡层)** 两张离屏纹理包装成的 [`slint::Image`]。两张图的用法见
    /// [`spawn_occluder_camera`]:UI 侧把它们夹着封面卡叠三层,卡片就被更近的粒子
    /// 逐像素挡住。返回裸元组而不是自定义结构体 —— `slint::Image` 是 ui 与 render3d
    /// 本就共有的类型,新造一个镜像结构体只是多一份要同步的字段。
    ///
    /// `width` / `height` 为窗口物理像素尺寸,与当前纹理不同就按需重建纹理
    /// (动态分辨率),0 尺寸忽略。纹理未就绪(首帧或刚重建)时返回空图;尺寸不变时
    /// 纹理身份稳定,只包装一次、之后复用 —— 内容每帧由 bevy 重画,Slint 重绘时实时采样。
    pub fn render_viz_frame(
        &mut self,
        frame: &VizFrame<'_>,
    ) -> (slint::Image, slint::Image) {
        let VizFrame {
            time,
            audio,
            cover,
            pointer,
            preset,
            width,
            height,
        } = *frame;
        if width > 0
            && height > 0
            && (width, height) != self.size
        {
            self.resize(width, height);
        }

        if !self.cloud_built {
            self.rebuild_viz_content();
            self.cloud_built = true;
        }

        // 过渡按播放页时钟推进 —— 门关着时钟不走,换歌渐变跟着定格,
        // 重开门从定格处继续。首帧没有上一帧可减,当作没走。
        let delta =
            self.last_time.map_or(0.0, |last| time - last);
        self.last_time = Some(time);
        self.transition.advance(delta);

        if let Some((w, h, rgba)) = cover {
            self.apply_cover(w, h, rgba);
        }

        self.apply_pointer(&pointer, delta);
        self.ripples.advance(delta);

        // 几何一动不动,一帧只换这一块 uniform:三万多颗粒子的位移在顶点
        // 着色器里算(见 docs/adr/0012)。
        let levels = cloud::band_levels(
            audio.get(..512).unwrap_or(&[]),
        );
        let color_mix = self.transition.color_mix();
        let burst = self.transition.burst();
        let ripple_count = self.ripples.active();
        let ripple_slots = self.ripples.pack();
        let object_scale =
            cloud::object_scale(self.size.0, self.size.1);
        if let Some(handle) = self.cloud_material.clone()
            && let Some(mut material) = self
                .app
                .world_mut()
                .resource_mut::<Assets<cloud::CloudMaterial>>()
                .get_mut(&handle)
        {
            material.params.time = time;
            material.params.bass = levels.bass;
            material.params.mid = levels.mid;
            material.params.treble = levels.treble;
            material.params.color_mix = color_mix;
            material.params.burst = burst;
            material.params.ripple_count = ripple_count;
            material.params.ripple_slots = ripple_slots;
            material.params.preset =
                cloud::preset_index(preset);
            // 物体类预设在竖屏会左右出画,按长宽比再收一档(见 cloud.rs)。
            material.params.object_scale = object_scale;
        }

        // 拖动转的是点云自己,不是相机 —— 相机一动遮挡层那台就得跟着动,
        // 两层还要逐像素对齐(见 cloud::Spin)。
        let (pitch, yaw) = self.spin.angles();
        if let Some(mut transform) =
            self.app
                .world_mut()
                .get_mut::<Transform>(self.root)
        {
            transform.rotation = Quat::from_euler(
                EulerRot::YXZ,
                yaw,
                pitch,
                0.0,
            );
        }

        // 遮挡层的深度门槛。用的是上一帧传播完的相机 GlobalTransform —— 相机全程不动,
        // 这一帧的门槛与当帧一致,不必为它多跑一次 transform 传播。
        let depth = self.anchor_depth();
        if let Some(mut cam3d) =
            self.app
                .world_mut()
                .get_mut::<Camera3d>(self.occluder_camera)
        {
            cam3d.depth_load_op =
                Camera3dDepthLoadOp::Clear(depth);
        }

        self.drive_and_finish(depth)
    }

    /// update 一帧并把两张离屏纹理(按身份缓存)包装成 Image 交回。
    fn drive_and_finish(
        &mut self,
        depth: f32,
    ) -> (slint::Image, slint::Image) {
        let t_update = Instant::now();
        self.app.update();
        self.perf +=
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
            log::info!(
                "render3d: 近 {PERF_WINDOW} 帧均耗时 —— app.update() {:.2}ms({}x{},遮挡门槛 {:.5})",
                self.perf / f64::from(PERF_WINDOW),
                self.size.0,
                self.size.1,
                depth,
            );
            self.perf = 0.0;
        }

        // 尺寸稳定时纹理身份稳定 → 只包装一次 Image,之后每帧由 bevy 重画内容,
        // Slint 重绘时实时采样同一张。
        let key = (tex.width(), tex.height());
        if self.image_key != Some(key) {
            let (w, h) = key;
            match slint::Image::try_from(tex) {
                Ok(img) => {
                    log::info!(
                        "render3d: 纹理就绪(第 {} 帧),{w}x{h} 已导入 Slint",
                        self.frames,
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
    /// 相机的投影长宽比由 bevy 每帧依据渲染目标尺寸自动更新,无需在此处理;**距离**也不必动 ——
    /// 粒子场是铺满视野的环境效果,竖视口不后撤,让粒子自然溢出画面上下(见
    /// [`BASE_CAMERA_POS`])。重建后 `image` 置空,下一帧重新把新纹理导入 Slint。
    fn resize(&mut self, width: u32, height: u32) {
        let new_target =
            make_target(&mut self.app, width, height);
        self.app
            .world_mut()
            .entity_mut(self.camera)
            .insert(RenderTarget::Image(
                new_target.clone().into(),
            ));

        // 遮挡层必须与场景图同尺寸同视角,否则两层对不上,遮挡会整体错位。
        let new_occluder =
            make_target(&mut self.app, width, height);
        self.app
            .world_mut()
            .entity_mut(self.occluder_camera)
            .insert(RenderTarget::Image(
                new_occluder.clone().into(),
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

    /// 把这一帧的指针状态变成涟漪与拖动。
    ///
    /// 指针**按住**时是拖动(转点云),没按住时划过就起涟漪 —— 与原版一致:
    /// `orbit.rotating` 那一支只转,不转的时候才 `queueParticlePointerFrame`。
    fn apply_pointer(
        &mut self,
        pointer: &Pointer,
        delta: f32,
    ) {
        if !pointer.active {
            self.last_pointer = None;
            self.spin.coast(delta);
            return;
        }
        let previous = self.last_pointer;
        self.last_pointer = Some((pointer.x, pointer.y));

        if pointer.down {
            // 拖动量按**物理像素**算,不然同一段手势在不同窗口大小下转得不一样多。
            if let Some((px, py)) = previous {
                let dx =
                    (pointer.x - px) * self.size.0 as f32;
                let dy =
                    (pointer.y - py) * self.size.1 as f32;
                self.spin.drag(dx, dy, delta);
            }
            return;
        }

        self.spin.coast(delta);
        // 指针没动就不再起新的一路 —— 停在那儿不动会把整张表刷成同一个点。
        if previous == Some((pointer.x, pointer.y)) {
            return;
        }
        if let Some((x, y)) =
            cloud::pointer_to_plane(pointer.x, pointer.y)
        {
            self.ripples.spawn(x, y);
        }
    }

    /// 换歌:把新封面的像素传成纹理挂上点云,旧的那张退成「上一首」,
    /// 并起一次过渡(颜色渐变 + burst)。
    ///
    /// 尺寸不合法(0 边、字节数与宽高对不上)就整帧忽略 —— 像素来自跨 crate 的
    /// seam,坏载荷不该让点云黑掉,更不该 panic。
    fn apply_cover(
        &mut self,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) {
        let expected =
            (width as usize) * (height as usize) * 4;
        if width == 0
            || height == 0
            || rgba.len() != expected
        {
            log::warn!(
                "render3d: 封面像素不合法({width}x{height},{} 字节),这一次不换",
                rgba.len()
            );
            return;
        }
        let Some(handle) = self.cloud_material.clone()
        else {
            return;
        };

        let image = Image::new(
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            rgba.to_vec(),
            // 封面是 sRGB 编码的;按线性读会整体发暗,点云的颜色就不是封面的颜色了。
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        );
        let next = self
            .app
            .world_mut()
            .resource_mut::<Assets<Image>>()
            .add(image);

        // 旧的那张退成「上一首」,渐变期间还看得见;再旧的那张这时才没人要。
        let rotated = {
            let mut materials = self
                .app
                .world_mut()
                .resource_mut::<Assets<cloud::CloudMaterial>>();
            let Some(mut material) =
                materials.get_mut(&handle)
            else {
                return;
            };
            let previous = material.cover.clone();
            let stale = material.prev_cover.clone();
            material.prev_cover = previous.clone();
            material.cover = next;
            material.params.has_cover = 1.0;
            (previous, stale)
        };

        // `stale == previous` 只在首曲成立:两者都还是那张占位图。此时既没有
        // 可渐变的旧封面(否则第一首歌会从一片纯白淡入),占位图也还挂在
        // `prev_cover` 上,不能释放。
        let (previous, stale) = rotated;
        if stale != previous {
            self.transition.start();
            self.app
                .world_mut()
                .resource_mut::<Assets<Image>>()
                .remove(&stale);
        }
    }

    /// 重建播放页封面点云:despawn 旧内容,把 [`cloud::CLOUD_GRID`]² 颗粒子烘成
    /// **一份** mesh,配一份自定义材质,spawn 成单个实体。
    ///
    /// 位移不在这里 —— 几何建好就不动,每帧只有材质的 uniform 在换
    /// (见 `docs/adr/0012`)。
    fn rebuild_viz_content(&mut self) {
        self.app
            .world_mut()
            .entity_mut(self.root)
            .despawn();

        let mesh = self
            .app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(cloud::build_cloud_mesh(
                cloud::cloud_vertices(),
            ));
        // 还没有封面时的占位图:`has_cover` 为 0,着色器走默认渐变色 ——
        // 点云在没有封面时不该消失。取到封面后由 `apply_cover` 换掉。
        let cover = self
            .app
            .world_mut()
            .resource_mut::<Assets<Image>>()
            .add(Image::default());
        let material = self
            .app
            .world_mut()
            .resource_mut::<Assets<cloud::CloudMaterial>>()
            .add(cloud::CloudMaterial {
                params: cloud::CloudParams::default(),
                prev_cover: cover.clone(),
                cover,
            });

        self.root = self
            .app
            .world_mut()
            .spawn((
                Mesh3d(mesh),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(CARD_ANCHOR),
            ))
            .id();
        self.cloud_material = Some(material);

        log::info!(
            "render3d: 封面点云重建完成,{0}x{0} = {1} 颗",
            cloud::CLOUD_GRID,
            cloud::CLOUD_GRID * cloud::CLOUD_GRID,
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

/// 摆放相机(渲染进离屏目标图),全程不变;返回相机实体供尺寸变化时改 RenderTarget。
///
/// 场景里没有光:点云的颜色直接取自封面纹理,不过光照(见 `cloud.wgsl`)。
fn spawn_camera(
    app: &mut App,
    target: &Handle<Image>,
) -> Entity {
    // 相机:渲染进离屏目标图,而非屏幕。0.19 起 RenderTarget 是独立组件,不再是 Camera 的字段。
    app.world_mut()
        .spawn((
            Camera3d::default(),
            Camera {
                // 粒子图要叠在 warp 背景上,没画到的像素必须透明。
                clear_color: ClearColorConfig::Custom(
                    Color::NONE,
                ),
                ..default()
            },
            RenderTarget::Image(target.clone().into()),
            // 默认的 TonyMcMapFace 需要 tonemapping_luts feature(会拉 LUT 资源)。
            // PoC 不启那个 feature,改用无需 LUT 的 None。要更好观感时再开该 feature。
            Tonemapping::None,
            // 位置全程不变,resize 也不动它(见 [`BASE_CAMERA_POS`])。
            Transform::from_translation(BASE_CAMERA_POS)
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
            // 必须与主相机同位同视角,否则两层对不上,遮挡会整体错位。
            Transform::from_translation(BASE_CAMERA_POS)
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

/// 相机位置,全程不变。
///
/// **正对**点云平面(y=0):偏一点就把那张平面看成俯视的梯形,封面就歪了。早先那层
/// 浮空尘埃是绕卡片飘的球,俯角只是给它一点立体感;点云是一张图,正对才是对的。
///
/// 刻意**不**随视口长宽比后撤:点云是铺满视野的环境效果,后撤只会把粒子缩成看不见的
/// 点(小米13 竖屏 aspect 0.45 会后撤 2.2 倍,真机上实测粒子直接消失)。恒用这个距离,
/// 让点云自然溢出画面四边。
const BASE_CAMERA_POS: Vec3 = Vec3::new(0.0, 0.0, 8.0);

#[cfg(test)]
mod tests {
    use super::*;

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
}
