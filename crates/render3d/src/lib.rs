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
//! 每帧产出**两张**图:粒子场本身,以及一张只含「比标注卡更近」的片元的遮挡层
//! (见 [`spawn_occluder_camera`])。UI 侧把二者夹着卡片叠三层,卡片就被粒子
//! 逐像素挡住 —— 深度正确的 UI。被标注的物体是 `marker` 那枚绕轨道走的方块;
//! 点云当不了它,理由写在 [`CLOUD_ORIGIN`] 下面那段。
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

mod aurorabtn;
mod navglass;
pub use aurorabtn::{
    AuroraBtnParams, AuroraBtnPass, AuroraBtnSlot,
};
pub use navglass::{NavGlassPass, NavParams};

mod warp;
pub use warp::{AUDIO_BYTES, WARP_SIDE, WarpPass};

mod cloud;
mod marker;
mod wall;
pub use wall::{
    WallCamera, WallCard, WallCover, WallFrame,
};

/// bevy 与 slint 共享的 `wgpu::Texture` 类型别名(经 slint 的 wgpu_29 再导出,与 bevy 同一份 crate)。
pub type SharedTexture = wgpu::Texture;

/// 一帧里视觉区的指针状态,POD。镜像 `ui::VizPointer`,apps/* 在 seam 处平凡拷过来。
///
/// 位置归一到 0..1(左上原点)。`active` 为假表示指针不在视觉区里,这一帧不拖动。
#[derive(Clone, Copy, Debug, Default)]
pub struct Pointer {
    pub x: f32,
    pub y: f32,
    pub down: bool,
    pub active: bool,
}

/// 点云封面这一帧的去向。镜像 `ui::viz::CoverUpdate`。
///
/// 三态而不是 `Option`:换歌与拿到新封面是两件事,中间隔着几百毫秒的网络,
/// 而封面常常根本拿不到。两者挤进一个 `Option`,点云就会一直挂着上一首
/// (见 `docs/adr/0014`)。
#[derive(Clone, Copy, Debug, Default)]
pub enum CoverUpdate<'a> {
    /// 没有新消息,保持现状。绝大多数帧都是这个。
    #[default]
    Unchanged,
    /// 换歌了,退回渐变。
    Clear,
    /// 新封面到了:(宽, 高, RGBA8)。
    Show(u32, u32, &'a [u8]),
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
    /// 这一帧点云的封面该怎么办。平帧恒为 [`CoverUpdate::Unchanged`]。
    pub cover: CoverUpdate<'a>,
    /// 视觉区里的指针。
    pub pointer: Pointer,
    /// 视觉预设的编号,越界回默认档。
    pub preset: i32,
    /// 这一帧要不要遮挡层。
    ///
    /// 遮挡层是逐像素深度合成的那一半(见 [`spawn_occluder_camera`]):有一张
    /// **深度卡片**要被场景挡住时才需要它。现在那张卡片是标注卡,挂在 `marker`
    /// 的前表面上,所以这里跟着「锚点在不在画面里」走。歌词不算,它画在粒子之上
    /// (见 `docs/adr/0010` 的「歌词是例外」)。
    ///
    /// 为假时那台相机整个关掉,不渲、不导入纹理。
    pub needs_occluder: bool,
    /// 窗口的物理像素尺寸。与当前纹理不同就按需重建(动态分辨率),0 尺寸忽略。
    pub width: u32,
    pub height: u32,
}

/// 离屏画面尺寸。固定分辨率,Slint 侧按面板大小缩放(见计划:先不做动态 resize)。
const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;

/// 每帧耗时日志的采样窗口(帧)。约两秒一行,够看趋势又不刷屏。
const PERF_WINDOW: u32 = 120;

/// 点云自己在世界里的位置。
///
/// 相机在 z = 8(见 [`BASE_CAMERA_POS`]),点云往镜头前挪这一截才有现在的取景大小。
///
/// 这个数原先叫 `CARD_ANCHOR`,一个常量同时当「点云放哪」与「深度卡片挂哪」用
/// (`76d3b73` 换成封面点云时借了 `71ce2f9` 的常量)。两者重合意味着锚点正落在
/// 点云正中心,也就是半数粒子在卡片前面 —— 恰恰是那份文档说过不能落的地方。
/// `needs_occluder` 一直恒假,这笔账在屏幕上从没露过面。现在拆开:点云的位置在
/// 这里,卡片的锚点跟着 `marker` 走。
const CLOUD_ORIGIN: Vec3 = Vec3::new(0.0, 0.0, 1.8);

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

    /// 播放页粒子场的一帧:本 crate 唯一的驱动入口。
    ///
    /// `time` 是播放页时钟(秒,门关即冻结,见 crates/ui);`audio` 是
    /// `spectrum` 布局的载荷(频谱行在前),只用前 512 字节拆频段;
    /// 粒子位置与缩放每帧由 [`particles::particle_pose`] 算好直写 Transform。
    ///
    /// 返回 **(场景, 遮挡层, 卡片锚点)**。两张图的用法见 [`spawn_occluder_camera`]:
    /// UI 侧把它们夹着标注卡叠三层,卡片就被更近的粒子逐像素挡住。锚点是
    /// [`CARD_ANCHOR_LOCAL`] 投到视口的归一化位置(见 [`anchor_viewport`]),
    /// `None` 表示这一帧它不在画面里,卡片该收起来。
    ///
    /// 返回裸元组而不是自定义结构体 —— `slint::Image` 是 ui 与 render3d 本就共有的
    /// 类型,新造一个镜像结构体只是多一份要同步的字段。
    ///
    /// `width` / `height` 为窗口物理像素尺寸,与当前纹理不同就按需重建纹理
    /// (动态分辨率),0 尺寸忽略。纹理未就绪(首帧或刚重建)时返回空图;尺寸不变时
    /// 纹理身份稳定,只包装一次、之后复用 —— 内容每帧由 bevy 重画,Slint 重绘时实时采样。
    pub fn render_viz_frame(
        &mut self,
        frame: &VizFrame<'_>,
    ) -> (slint::Image, slint::Image, Option<(f32, f32)>) {
        let VizFrame {
            time,
            audio,
            cover,
            pointer,
            preset,
            needs_occluder,
            width,
            height,
        } = *frame;
        if width > 0
            && height > 0
            && (width, height) != self.size
        {
            self.resize(width, height);
        }

        // 点云与卡墙互斥:播放页开着就不该有人渲卡墙,反之亦然。
        // 各自的入口把对方的相机关掉,谁也不会白渲一整面。
        self.wall.set_active(&mut self.app, false);
        if let Some(mut cam) = self
            .app
            .world_mut()
            .get_mut::<Camera>(self.camera)
        {
            cam.is_active = true;
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

        match cover {
            CoverUpdate::Unchanged => {}
            CoverUpdate::Clear => self.clear_cover(),
            CoverUpdate::Show(w, h, rgba) => {
                self.apply_cover(w, h, rgba);
            }
        }

        self.apply_pointer(&pointer, delta);

        // 几何一动不动,一帧只换这一块 uniform:三万多颗粒子的位移在顶点
        // 着色器里算(见 docs/adr/0012)。
        let levels = cloud::band_levels(
            audio.get(..512).unwrap_or(&[]),
        );
        let color_mix = self.transition.color_mix();
        let burst = self.transition.burst();
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
            material.params.preset =
                cloud::preset_index(preset);
            // 物体类预设在竖屏会左右出画,按长宽比再收一档(见 cloud.rs)。
            material.params.object_scale = object_scale;
        }

        // 拖动转的是点云自己,不是相机 —— 相机一动遮挡层那台就得跟着动,
        // 两层还要逐像素对齐(见 cloud::Spin)。
        let (pitch, yaw) = self.spin.angles();
        let rotation =
            Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0);
        if let Some(mut transform) =
            self.app
                .world_mut()
                .get_mut::<Transform>(self.root)
        {
            transform.rotation = rotation;
        }

        // 没有深度卡片就整台相机关掉:不渲、不导入纹理。这是第二次全场景绘制,
        // 白渲一帧就是白花一帧(见 VizFrame::needs_occluder)。
        if let Some(mut cam) = self
            .app
            .world_mut()
            .get_mut::<Camera>(self.occluder_camera)
        {
            cam.is_active = needs_occluder;
        }

        // 标记体沿轨道走这一帧。用播放页时钟而不是墙钟:门关着时钟不走,
        // 方块跟着定格,重开门从原处继续 —— 与换歌过渡同一套时基。
        let marker_pose = marker::pose(time);
        if let Some(mut transform) = self
            .app
            .world_mut()
            .get_mut::<Transform>(self.marker)
        {
            *transform = marker_pose;
        }

        // 锚点一次投影,深度门槛与卡片挂点都从它出来。
        let ndc = self.anchor_ndc(&marker_pose);
        let depth = occluder_depth(ndc);
        if let Some(mut cam3d) =
            self.app
                .world_mut()
                .get_mut::<Camera3d>(self.occluder_camera)
        {
            cam3d.depth_load_op =
                Camera3dDepthLoadOp::Clear(depth);
        }

        let (scene, occluder) =
            self.drive_and_finish(depth, needs_occluder);
        (scene, occluder, anchor_viewport(ndc))
    }

    /// 卡墙的一帧(docs/adr/0025):位姿与相机由 `ui::wall` 算好传入,
    /// 这里摆进场景、渲一帧、交回纹理。与点云互斥 —— 本入口把点云的
    /// 两台相机关掉,只亮卡墙那台。省电门在 ui 侧,静止的墙不会调到这。
    pub fn render_wall_frame(
        &mut self,
        frame: &wall::WallFrame,
    ) -> slint::Image {
        self.wall.set_active(&mut self.app, true);
        for cam in [self.camera, self.occluder_camera] {
            if let Some(mut c) = self
                .app
                .world_mut()
                .get_mut::<Camera>(cam)
            {
                c.is_active = false;
            }
        }
        self.wall.apply(&mut self.app, frame);
        self.app.update();
        self.wall.finish(&self.app)
    }

    /// update 一帧并把两张离屏纹理(按身份缓存)包装成 Image 交回。
    ///
    /// `needs_occluder` 为假时遮挡层那半整个跳过 —— 相机已经关了,纹理里
    /// 是上一次的残留,导进去只会让 UI 拿到一张过期的图。
    fn drive_and_finish(
        &mut self,
        depth: f32,
        needs_occluder: bool,
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
            return self.frame_images(needs_occluder);
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
        if needs_occluder
            && let Some(tex) =
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

        self.frame_images(needs_occluder)
    }

    /// 当前这一帧交给 UI 的两张图:(场景, 遮挡层)。任一未就绪时给空图。
    fn frame_images(
        &self,
        needs_occluder: bool,
    ) -> (slint::Image, slint::Image) {
        (
            self.image.clone().unwrap_or_default(),
            // 不需要就给空图。UI 侧的 `viz-occluder.width > 0` 守卫据此
            // 不摆那一层 —— 给一张过期的图会让卡片被上一帧的粒子挡着。
            if needs_occluder {
                self.occluder_image
                    .clone()
                    .unwrap_or_default()
            } else {
                slint::Image::default()
            },
        )
    }

    /// 卡片锚点这一帧在主相机里的 NDC。**一次投影,两个去处**:z 给遮挡层当深度
    /// 门槛(见 [`occluder_depth`]),xy 给标注卡当挂点(见 [`anchor_viewport`])。
    ///
    /// `marker_pose` 是这一帧刚写进标记体的位姿,而不是从 `GlobalTransform` 读回来的 ——
    /// 传播要等 `app.update()`,读它拿到的是上一帧的位置,卡片会慢半拍跟在方块后面。
    /// 标记体没有父实体,`GlobalTransform` 与 `Transform` 恒等,直接用就是准的。
    fn anchor_ndc(
        &self,
        marker_pose: &Transform,
    ) -> Option<Vec3> {
        let camera_entity =
            self.app.world().entity(self.camera);
        let (Some(camera), Some(camera_pose)) = (
            camera_entity.get::<Camera>(),
            camera_entity.get::<GlobalTransform>(),
        ) else {
            return None;
        };
        camera.world_to_ndc(
            camera_pose,
            marker::front_face(marker_pose),
        )
    }

    /// 从 bevy 的渲染子世界里取出某张离屏目标图对应的 `wgpu::Texture`。
    fn extract_texture(
        &self,
        handle: &Handle<Image>,
    ) -> Option<SharedTexture> {
        extract_texture(&self.app, handle)
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
    }

    /// 换歌那一刻:点云退回渐变,不挂着上一首的封面等新图。
    ///
    /// 只把 `has_cover` 落回 0 —— 着色器据此走默认渐变色,纹理本身留着不动:
    /// 换下一首时 [`Self::apply_cover`] 会把它轮成「上一首」,渐变过渡还要用。
    ///
    /// 不起过渡:这不是"换到另一张封面",而是"暂时没有封面",没有可渐变的两端。
    fn clear_cover(&mut self) {
        let Some(handle) = self.cloud_material.clone()
        else {
            return;
        };
        let mut materials = self
            .app
            .world_mut()
            .resource_mut::<Assets<cloud::CloudMaterial>>();
        if let Some(mut material) =
            materials.get_mut(&handle)
        {
            material.params.has_cover = 0.0;
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
                Transform::from_translation(CLOUD_ORIGIN),
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

/// 从 bevy 的渲染子世界里取出某张离屏目标图对应的 `wgpu::Texture`。
/// 自由函数版:卡墙的 `WallScene` 也要用,而它拿不到整个 `Scene`。
fn extract_texture(
    app: &App,
    handle: &Handle<Image>,
) -> Option<SharedTexture> {
    let gpu_images = app
        .get_sub_app(RenderApp)?
        .world()
        .get_resource::<RenderAssets<GpuImage>>()?;
    let gpu_image = gpu_images.get(handle)?;
    // GpuImage.texture: render_resource::Texture, Deref 到 wgpu::Texture。
    Some((*gpu_image.texture).clone())
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
/// 锚点在视口里的归一化位置(0..1,**左上**原点),出画或投影不出来时为 `None`。
///
/// 归一而非物理像素:离屏纹理尺寸与 UI 的逻辑像素是两套刻度,交给 UI 侧乘自己的
/// 面板尺寸,中间少一次要同步的换算。
///
/// 出画就给 `None`,不钳到边上:钳过的卡片会粘在屏幕边缘假装还指着那个物体。
fn anchor_viewport(
    anchor_ndc: Option<Vec3>,
) -> Option<(f32, f32)> {
    let ndc = anchor_ndc?;
    // 深度判据与遮挡层那条共用:锚点跑到相机背后或视锥之外,这一帧就没有卡片。
    if !ndc.is_finite() || !(0.0..=1.0).contains(&ndc.z) {
        return None;
    }
    // NDC 的 y 向上、UI 的 y 向下,所以 y 这一路取负。
    let x = ndc.x * 0.5 + 0.5;
    let y = 0.5 - ndc.y * 0.5;
    let on_screen = (0.0..=1.0).contains(&x)
        && (0.0..=1.0).contains(&y);
    on_screen.then_some((x, y))
}

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
    // 取投影矩阵的 trait,不在 prelude 里。
    use bevy::camera::CameraProjection;

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

    /// 锚点在视锥内:除了深度,还要给出卡片挂在视口哪一点。
    /// 归一到 0..1,**y 轴翻转** —— NDC 的 y 向上,UI 的 y 向下,不翻卡片就上下颠倒。
    #[test]
    fn an_anchor_in_front_of_the_camera_projects_to_a_viewport_point() {
        assert_eq!(
            anchor_viewport(Some(Vec3::new(0.0, 0.0, 0.5))),
            Some((0.5, 0.5)),
            "画面正中"
        );
        // NDC 左上角 (-1, 1) 对应视口 (0, 0):y 翻过来了。
        assert_eq!(
            anchor_viewport(Some(Vec3::new(-1.0, 1.0, 0.0))),
            Some((0.0, 0.0))
        );
        // NDC 右下角 (1, -1) 对应视口 (1, 1)。
        assert_eq!(
            anchor_viewport(Some(Vec3::new(1.0, -1.0, 1.0))),
            Some((1.0, 1.0))
        );
    }

    /// 边界:锚点在相机背后(投影给不出值)或深度越界时,卡片这一帧不显示。
    /// 与遮挡层退回空是同一个判据 —— 卡片藏起来的那一帧,不该还留着挡它的那一层。
    #[test]
    fn an_anchor_behind_the_camera_has_no_viewport_point() {
        assert_eq!(anchor_viewport(None), None);
        for z in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
            assert_eq!(
                anchor_viewport(Some(Vec3::new(0.0, 0.0, z))),
                None,
                "NDC z = {z} 的锚点不该挂出卡片"
            );
        }
    }

    /// 边界:锚点在画面左右(或上下)之外时同样不显示。
    /// 竖屏视口只看得到封面平面中间一条,锚点转出画面是常态,不是异常;
    /// 画到界外会糊在播放页别的控件上。
    #[test]
    fn an_anchor_off_the_side_of_the_screen_has_no_viewport_point() {
        for (x, y) in
            [(-1.2, 0.0), (1.5, 0.0), (0.0, -2.0), (0.0, 1.01)]
        {
            assert_eq!(
                anchor_viewport(Some(Vec3::new(x, y, 0.5))),
                None,
                "NDC ({x}, {y}) 已经出画,卡片该藏起来"
            );
        }
    }

    /// 一圈取多少个采样点。够密到能逮住轨道端点,又不至于让单测变慢。
    const ORBIT_STEPS: u32 = 72;

    /// 轨道上第 `step` 个采样点对应的播放页时钟。
    fn orbit_time(step: u32) -> f32 {
        marker::ORBIT_PERIOD * f32::from(
            u16::try_from(step).expect("采样点数远小于 u16 上限"),
        ) / ORBIT_STEPS as f32
    }

    /// 标记体绕的那条轨道,近端离开封面平面、远端贴回去,且始终在相机这一侧。
    /// 远端贴平面才有粒子成片从它前面过(遮挡演得出来),近端离开平面卡片才读得成。
    #[test]
    fn the_marker_orbits_between_the_cover_plane_and_the_camera() {
        let (mut nearest, mut farthest) =
            (f32::MIN, f32::MAX);
        for step in 0..ORBIT_STEPS {
            let z = marker::pose(orbit_time(step))
                .translation
                .z;
            nearest = nearest.max(z);
            farthest = farthest.min(z);
        }

        // 远端贴着封面平面,但不穿到平面后面 —— 穿过去就再没有粒子能挡在前面,
        // 遮挡反而演不出来了。
        let gap = farthest - CLOUD_ORIGIN.z;
        assert!(
            gap > 0.0,
            "轨道远端 {farthest} 跑到封面平面 {} 后面了",
            CLOUD_ORIGIN.z
        );
        // 远端离平面多远才算「粒子够得着」,判据是粒子 z 位移的峰值(1.2~1.5,
        // 见 cloud.wgsl 的 place_cover):离得远小于峰值,就有成片的粒子穿过去。
        // 0.5 这个上限比峰值小一半有余,留足余量。
        assert!(
            gap < 0.5,
            "轨道远端离平面 {gap},粒子够不着,遮挡演不出来"
        );

        // 近端要高过粒子 z 位移的峰值(约 1.2~1.5,见 cloud.wgsl 的 place_cover),
        // 卡片在这一段才干净。
        assert!(
            nearest - CLOUD_ORIGIN.z > 1.4,
            "轨道近端只离平面 {},粒子还会糊在卡片上",
            nearest - CLOUD_ORIGIN.z
        );
        assert!(
            nearest < BASE_CAMERA_POS.z,
            "轨道近端 {nearest} 跑到相机后面了"
        );
    }

    /// 一个周期正好转一圈,且四分之一周期就是四分之一圈 —— 卡片的移动由它驱动,
    /// 转快转慢是观感,转不满一圈是错。
    #[test]
    fn the_marker_turns_once_per_period() {
        let start = marker::pose(0.0).translation;
        // 起点在轨道最近端(正对相机那一侧)。
        assert!(
            start.abs_diff_eq(
                marker::ORBIT_CENTER
                    + Vec3::Z * marker::ORBIT_RADIUS,
                1e-4
            ),
            "起点该在轨道近端,实际 {start}"
        );
        assert!(
            marker::pose(marker::ORBIT_PERIOD)
                .translation
                .abs_diff_eq(start, 1e-4),
            "转满一个周期该回到起点"
        );

        // 四分之一圈到侧面,半圈到最远端。
        assert!(
            marker::pose(marker::ORBIT_PERIOD / 4.0)
                .translation
                .abs_diff_eq(
                    marker::ORBIT_CENTER
                        + Vec3::X * marker::ORBIT_RADIUS,
                    1e-4
                )
        );
        assert!(
            marker::pose(marker::ORBIT_PERIOD / 2.0)
                .translation
                .abs_diff_eq(
                    marker::ORBIT_CENTER
                        - Vec3::Z * marker::ORBIT_RADIUS,
                    1e-4
                )
        );
    }

    /// 卡片锚在标记体前表面**之前**,不是中心、也不是前表面本身。
    ///
    /// 中心不行:方块自己的前半比锚点更近,会被画进遮挡层、盖住自己的标签。
    /// 前表面本身也不行,这是 2026-08-13 真机实拍到的 —— 深度测试是
    /// `GreaterEqual`,含等号,前表面的片元深度恰好等于门槛就照样通过,
    /// 方块把标签盖掉了大半。
    #[test]
    fn the_card_anchor_clears_the_marker_front_face() {
        for step in 0..ORBIT_STEPS {
            let pose = marker::pose(orbit_time(step));
            // 不自转:一旦有姿态,前表面就不再是 +z 那面,锚点会飘进方块里。
            assert_eq!(
                pose.rotation,
                Quat::IDENTITY,
                "标记体不该自转"
            );
            let anchor = marker::front_face(&pose);
            let offset = anchor - pose.translation;
            assert!(
                offset.x.abs() < 1e-5 && offset.y.abs() < 1e-5,
                "锚点该正对前表面中心,实际横向偏了 {offset}"
            );
            // 判据是「清出一段间隙」而不是「大于」。裸的 `>` 逮不住把锚点放回
            // 前表面上的写法:`(t + h) - t` 的舍入误差本来就可能落在 h 之上一个
            // ulp,于是那条断言恒真(这一点是变异检验实测出来的)。
            assert!(
                offset.z > marker::MARKER_HALF * 1.05,
                "锚点只到 {},没从前表面 {} 清出间隙 —— 等号会让方块盖住自己的标签",
                offset.z,
                marker::MARKER_HALF
            );
        }
    }

    /// 拿 bevy **自己的**投影矩阵把锚点沿整条轨道走一圈:每一处都得挂得出卡片。
    ///
    /// 这条守的是上面几条守不住的那件事 —— 它们喂的是手写的 NDC,而真相机上锚点
    /// 若恒在画面外,那几条照样全绿、屏幕上什么都没有。这里复刻
    /// `Camera::world_to_ndc` 的算法(裁剪矩阵 × 相机逆变换,再做透视除),
    /// 不需要 GPU 也不需要 `App`,量的是真几何。
    #[test]
    fn the_card_anchor_stays_on_screen_through_a_full_orbit() {
        // 小米13 竖屏,三端里最窄的那个视口 —— 横向可视范围最小,最容易把锚点甩出去。
        const PORTRAIT_ASPECT: f32 = 1080.0 / 2400.0;

        let projection = PerspectiveProjection {
            aspect_ratio: PORTRAIT_ASPECT,
            ..Default::default()
        };
        let camera =
            Transform::from_translation(BASE_CAMERA_POS)
                .looking_at(Vec3::ZERO, Vec3::Y);
        let clip_from_world = projection.get_clip_from_view()
            * camera.to_matrix().inverse();

        for step in 0..ORBIT_STEPS {
            let time = orbit_time(step);
            let anchor =
                marker::front_face(&marker::pose(time));
            let ndc =
                clip_from_world.project_point3(anchor);
            assert!(
                anchor_viewport(Some(ndc)).is_some(),
                "t = {time}s 时锚点出画了,世界坐标 {anchor},NDC = {ndc}"
            );
        }
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
