//! 一个自持的 bevy 离屏渲染场景:它的字段、常量,以及一次性搭建。

use std::sync::Arc;

use bevy::prelude::*;
// BSN(next-gen 场景系统,bevy_scene feature)的 bsn! 宏、Scene/SceneList、
// World::spawn_scene 都已在 bevy::prelude 里,无需额外 use。见 rebuild_content。
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。
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

use crate::camera::{
    BASE_CAMERA_POS, spawn_camera, spawn_occluder_camera,
};
use crate::seam::SharedTexture;
use crate::{cloud, marker, wall};

mod cover;
mod input;
mod target;
mod viz;

/// 离屏画面尺寸。固定分辨率,Slint 侧按面板大小缩放(见计划:先不做动态 resize)。
pub(crate) const WIDTH: u32 = 320;

pub(crate) const HEIGHT: u32 = 240;

/// 每帧耗时日志的采样窗口(帧)。约两秒一行,够看趋势又不刷屏。
pub(crate) const PERF_WINDOW: u32 = 120;

/// 点云自己在世界里的位置。
///
/// 相机在 z = 8(见 [`BASE_CAMERA_POS`]),点云往镜头前挪这一截才有现在的取景大小。
///
/// 这个数原先叫 `CARD_ANCHOR`,一个常量同时当「点云放哪」与「深度卡片挂哪」用
/// (`76d3b73` 换成封面点云时借了 `71ce2f9` 的常量)。两者重合意味着锚点正落在
/// 点云正中心,也就是半数粒子在卡片前面 —— 恰恰是那份文档说过不能落的地方。
/// `needs_occluder` 一直恒假,这笔账在屏幕上从没露过面。现在拆开:点云的位置在
/// 这里,卡片的锚点跟着 `marker` 走。
pub(crate) const CLOUD_ORIGIN: Vec3 =
    Vec3::new(0.0, 0.0, 1.8);

// 标注卡挂在标记体的前表面上,几何全在 `marker` 模块里。
//
// 曾经挂在封面平面上(点云 root 的局部坐标 (0.8, 0.8, 0)),想让它跟着拖动自转走。
// 真机上那张卡片一像素都看不见:锚点落在平面上,意味着半数粒子比它更近、全被画进
// 遮挡层,把卡片整块糊掉 —— 正是 `CLOUD_ORIGIN` 那段历史里警告过的同一个错。
//
// 而且这不是调个数能解决的。要卡片读得成,锚点得抬到粒子位移峰值(1.2~1.5)之上;
// 要它刚性挂在点云上还留在画面里,它到旋转中心的距离又不能超过竖屏可见半宽 1.156。
// 两个条件互斥。结论是点云当不了被标注物 —— 它没有「一个东西」可指,于是有了
// `marker`:一枚自己绕轨道走的方块,深度确定、边界可指,遮挡不必等人来拖。

// 编译期钉死轨道与封面平面的关系:远端要贴着平面(粒子才穿得过去),
// 近端要离开平面(卡片才读得成)。两边都是常量,不必进单测。
const _: () = assert!(
    marker::ORBIT_CENTER.z - marker::ORBIT_RADIUS
        > CLOUD_ORIGIN.z
);
const _: () = assert!(
    marker::ORBIT_CENTER.z + marker::ORBIT_RADIUS
        < BASE_CAMERA_POS.z
);

/// 空遮挡层对应的深度清除值:近平面。反向 Z 下没有片元比近平面更近,这一层因此全空。
pub(crate) const EMPTY_OCCLUDER_DEPTH: f32 = 1.0;

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
    /// 被标注的那枚方块(见 `marker`)。每帧沿轨道挪一次,卡片锚在它前表面上。
    marker: Entity,
    /// 点云材质的句柄:每帧改它的 uniform(时间、三段电平),几何一动不动。
    cloud_material: Option<Handle<cloud::CloudMaterial>>,
    /// 换歌过渡(颜色渐变 + burst)。按播放页时钟推进。
    transition: cloud::TrackTransition,
    /// 拖动带来的点云自转与松手后的惯性。
    spin: cloud::Spin,
    /// 上一帧的指针状态,用来算这一帧拖了多少。
    last_pointer: Option<(f32, f32)>,
    /// 上一帧的播放页时钟,用来算过渡要推进多少。门关着时钟不走,过渡跟着定格。
    last_time: Option<f32>,
    /// 渲染到离屏图的相机。尺寸变化时要改它的 RenderTarget 指向新纹理。
    camera: Entity,
    /// 遮挡层的离屏目标图:同一个场景,但只留比卡片锚点更近的片元,其余透明。
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
    /// 卡墙那一摊(自己的相机、目标纹理、卡实体池),见 `wall.rs`。
    wall: wall::WallScene,
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
        // 卡墙上「正在放的那一张」的闪卡材质,同一套内嵌路数。
        bevy::asset::embedded_asset!(app, "foil.wgsl");
        app.add_plugins(MaterialPlugin::<
            crate::foil::FoilMaterial,
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
        // 被标注的方块。不挂 RenderLayers,于是它在默认层上:点云的两台相机
        // 看得见,卡墙那台(layer 1)看不见。
        let marker = marker::spawn(&mut app);
        // 7) 卡墙:自己的相机与目标,初始不激活(见 wall.rs)。
        let wall_scene = wall::WallScene::new(&mut app);

        // 手动驱动模式下,首帧前要走完插件的 finish/cleanup(平时由 App::run 的 runner 负责)。
        app.finish();
        app.cleanup();

        Self {
            app,
            device: device.clone(),
            queue: queue.clone(),
            target,
            root,
            marker,
            cloud_material: None,
            transition: cloud::TrackTransition::default(),
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
            wall: wall_scene,
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
}

/// 造一张 bevy 离屏渲染目标图(Rgba8Unorm,满足 Slint 导入要求:格式 +
/// TEXTURE_BINDING|RENDER_ATTACHMENT),返回其资源句柄。
pub(crate) fn make_target(
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

/// 从 bevy 的渲染子世界里取出某张离屏目标图对应的 `wgpu::Texture`。
/// 自由函数版:卡墙的 `WallScene` 也要用,而它拿不到整个 `Scene`。
pub(crate) fn extract_texture(
    app: &App,
    handle: &Handle<Image>,
) -> Option<SharedTexture> {
    let gpu_images =
        app.get_sub_app(RenderApp)?
            .world()
            .get_resource::<RenderAssets<GpuImage>>()?;
    let gpu_image = gpu_images.get(handle)?;
    // GpuImage.texture: render_resource::Texture, Deref 到 wgpu::Texture。
    Some((*gpu_image.texture).clone())
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}
