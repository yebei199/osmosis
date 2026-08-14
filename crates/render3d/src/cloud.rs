//! 播放页封面点云的**纯计算**:频段拆分与点云网格的顶点数据。
//!
//! 运动学不在这里 —— 三万多颗粒子的位移全在 `cloud.wgsl` 的顶点着色器里
//! (见 `docs/adr/0012`),CPU 每帧只更新几个 uniform。本模块只回答两件事:
//! 频谱行怎么拆成三段电平,以及那份烘一次就不动的顶点缓冲长什么样。
//!
//! 行为模型照抄 Mineradio 的封面粒子系统(源码
//! `public/js/modules/02-visual/00-pointer-cover-particles.js`)的 **preset 0
//! (SILK)**,也就是它的默认档。

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, MeshVertexBufferLayoutRef};
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, PrimitiveTopology,
    RenderPipelineDescriptor, ShaderType,
    SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

mod mesh;

mod spin;

mod transition;

pub(crate) use mesh::{
    CUBE_INDICES, CUBE_VERTICES, CloudVertices, Levels,
    band_levels, cloud_vertices,
};

pub(crate) use spin::Spin;

pub(crate) use transition::TrackTransition;

/// 出轮廓,但一眼就知道它是由方块砌的。
///
/// 顺带把粒子数砍到 1/16(14.7 万 → 9216),这才养得起把每颗从方片换成立方体。
pub(crate) const CLOUD_GRID: usize = 96;

/// 点云平面在世界里的边长,同原版。
///
/// 曾经放大到 12 让它溢出视口,但铺满四边之后点云不再像「一张图」,只像一层贴在
/// 屏幕上的网点。收回原版这一档:画面中间一块有边界的封面,四周留给背景。
/// 圆点大小按格距走(见 [`CELL_WORLD`]),平面缩回来点也跟着变细。
pub(crate) const PLANE_SIZE: f32 = 4.8;

/// 原版的平面边长。位移的绝对幅度都是照它调的,平面放大后要同比放大位移
/// (见 [`MOTION_SCALE`]),否则起伏相对整片点云就缩水了。
const ORIGINAL_PLANE_SIZE: f32 = 4.8;

/// 平面相对原版放大了多少倍。
///
/// 所有预设的几何都是照原版那块 4.8 的平面调的,整体乘上这个倍数就搬到我们的尺度上。
/// 位移也一样 —— 位移是世界单位,平面放大了它不跟着放大,起伏相对整片点云就缩水。
const PLANE_SCALE: f32 = PLANE_SIZE / ORIGINAL_PLANE_SIZE;

/// 原版的相机环绕半径(`orbit.userRadius`)。
const ORIGINAL_CAMERA_RADIUS: f32 = 6.6;

/// 「物体」类预设(滚筒/星球/唱片)的尺度倍数。
///
/// 这几档是被相机取景的**一个物体**,不是铺满视野的一层 —— 该按相机距离对齐,
/// 不是按平面大小。乘上平面倍数的话球体会大到糊在镜头上(实测)。
/// 封面档自己就在放大后的格点上,星河档的坐标本来就有二三十个单位,两者都不用它。
const OBJECT_SCALE: f32 = 8.0 / ORIGINAL_CAMERA_RADIUS;

/// 物体类预设这一帧的尺度倍数:基准倍数 × 窄视口的收缩。
///
/// 透视投影固定的是**垂直**视野,横向可视 = 纵向 × 长宽比。竖屏(小米13 是 0.45)
/// 横向只剩不到一半,球体与唱片就左右出画 —— 真机上实测星球被切掉两边。
/// 相机**不后撤**(那会把铺满视野的两档缩成看不见的点,见 `BASE_CAMERA_POS`),
/// 改成只把物体类那三档按长宽比收一档:横屏不动,竖屏跟着窄下去。
pub(crate) fn object_scale(width: u32, height: u32) -> f32 {
    if width == 0 || height == 0 {
        return OBJECT_SCALE;
    }
    let aspect = width as f32 / height as f32;
    OBJECT_SCALE * aspect.min(1.0)
}

/// 律动强度,同原版的 `uIntensity` 滑块。
///
/// 原版默认 0.85,这里调到 1.36 —— 这一层是播放页的主视觉,起伏跟着节奏走得明显
/// 才有意思;原版那档是给「UI 后面一块矩形」调的。面板上限是 1.6。
const INTENSITY: f32 = 1.36;

/// 视觉预设的个数。与 `cloud.wgsl` 里的分支手工对齐。
///
/// **只剩 0「封面」一档。** 曾经的滚筒/星球/虚空/唱片/星河已删 —— 粒子离开封面
/// 平面之后那张图就散了,而点云存在的理由正是封面本身(见 `docs/adr/0014`)。
/// 编号这条 seam 留着,加档时这里加数、着色器里把 switch 加回来。
pub(crate) const PRESET_COUNT: i32 = 1;

/// 界面给的预设编号 → 着色器认得的下标。越界回默认档。
///
/// 编号来自 `.slint`,是跨层的外部输入;给个不存在的档不该画出一片空白。
pub(crate) fn preset_index(requested: i32) -> u32 {
    if (0..PRESET_COUNT).contains(&requested) {
        requested as u32
    } else {
        0
    }
}

/// 一份点云 mesh 的顶点数据:每颗粒子一个**立方体**。
///
/// 立方体而不是朝向相机的方片:方片没有朝向,永远是一个正对镜头的色块,起伏
/// 再大也只是一张会动的图。立方体有六个面,面与面明暗不同,点云高低错落时能

/// 片元 alpha 的丢弃门槛与混合模式,分平台。
///
/// 桌面走软边发光圆点(几乎不丢弃 + `Blend`);安卓走硬边(0.5 + `Mask`)——
/// 小米13(Adreno)上半透明小元素整片不显示是本仓踩过的坑,见 `docs/adr/0012`。
#[cfg(target_os = "android")]
const ALPHA_CUTOFF: f32 = 0.5;
#[cfg(not(target_os = "android"))]
const ALPHA_CUTOFF: f32 = 0.01;

/// 相邻两个格点在世界里的间距。圆点大小按它定,不按像素定。
///
/// 钉像素尺寸的话,视口一变点与缝的比例就跑掉了(窗口拉大 → 格距变大 → 点相对变小,
/// 点云散成一片看不出图)。格距与点径同在世界单位里,投影之后比例恒定。
const CELL_WORLD: f32 =
    PLANE_SIZE / (CLOUD_GRID as f32 - 1.0);

/// 立方体边长占一格的比例。
///
/// 小于 1,方块之间留出缝 —— 缝是像素画的一部分:没有缝就砌成一整块,又变回
/// 「一张会动的图」。0.82 留下约两成的缝,方块看得清、图也还连得起来。
const POINT_FILL: f32 = 0.82;

/// 点云材质的 uniform 块,逐字段镜像 `cloud.wgsl` 的 `CloudParams`。
#[derive(Clone, Copy, ShaderType)]
pub(crate) struct CloudParams {
    pub time: f32,
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
    pub intensity: f32,
    /// 位移幅度的整体倍数,见 [`PLANE_SCALE`]。
    pub plane_scale: f32,
    /// 物体类预设的尺度倍数,见 [`OBJECT_SCALE`] 与 [`narrow_viewport_scale`]。
    pub object_scale: f32,
    /// 当前视觉预设的下标,见 [`preset_index`]。
    pub preset: u32,
    pub has_cover: f32,
    pub alpha_cutoff: f32,
    /// 圆点半径,世界单位。见 [`CELL_WORLD`] / [`POINT_FILL`]。
    pub point_radius: f32,
    /// 新旧封面的混合进度:0 = 全旧,1 = 全新。见 [`TrackTransition`]。
    pub color_mix: f32,
    /// 换歌脉冲强度:粒子外扩再归位。同上。
    pub burst: f32,
}

/// 静止、无封面的一帧参数:常量项按原版默认档,音频项全零。
impl Default for CloudParams {
    fn default() -> Self {
        Self {
            time: 0.0,
            bass: 0.0,
            mid: 0.0,
            treble: 0.0,
            intensity: INTENSITY,
            plane_scale: PLANE_SCALE,
            object_scale: OBJECT_SCALE,
            preset: 0,
            has_cover: 0.0,
            alpha_cutoff: ALPHA_CUTOFF,
            point_radius: CELL_WORLD * POINT_FILL * 0.5,
            // 没有「上一首」可渐变,直接全新。
            color_mix: 1.0,
            burst: 0.0,
        }
    }
}

/// 点云的自定义材质:顶点位移 + 软边圆点全在 `cloud.wgsl` 里。
///
/// 走 bevy 的材质管线(而不是自起一个 wgpu pass)是为了自动同时进两台相机,
/// 白拿既有的遮挡设施 —— 理由见 `docs/adr/0012`。
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub(crate) struct CloudMaterial {
    #[uniform(0)]
    pub params: CloudParams,
    /// 当前曲目的封面。`params.has_cover` 为 0 时内容无所谓(占位图)。
    #[texture(1)]
    #[sampler(2)]
    pub cover: Handle<Image>,
    /// 上一首的封面,只在换歌渐变期间还看得见。首曲与它自己同一张。
    #[texture(3)]
    #[sampler(4)]
    pub prev_cover: Handle<Image>,
}

impl Material for CloudMaterial {
    fn vertex_shader() -> ShaderRef {
        SHADER_PATH.into()
    }

    fn fragment_shader() -> ShaderRef {
        SHADER_PATH.into()
    }

    /// 两端都走 alpha 测试,而不是桌面用混合。
    ///
    /// 混合模式下 bevy 不写深度,而立方体是**有体积**的:不写深度就没有前后
    /// 之分,起伏大的时候后排的方块会盖到前排上。alpha 测试写深度,遮挡才对。
    /// 代价是半透明档(星河)从半透变成实心 —— 立方体本就不该是半透的。
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Mask(ALPHA_CUTOFF)
    }

    /// 关掉 prepass 与阴影:两者都会用**默认**的顶点着色器再跑一遍这份 mesh,
    /// 而我们的顶点布局(角偏移塞在法线位上)只有 `cloud.wgsl` 读得懂。
    /// 点云也不需要投影 —— 它是装饰层,场景里没有承影面。
    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    /// 声明顶点布局:位置、角偏移(借法线位)、采样 uv,与 `Vertex` 的
    /// `@location` 一一对应。不声明的话管线按默认 mesh 布局走,对不上。
    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.vertex.buffers = vec![
            layout.0.get_layout(&[
                Mesh::ATTRIBUTE_POSITION
                    .at_shader_location(0),
                Mesh::ATTRIBUTE_NORMAL
                    .at_shader_location(1),
                Mesh::ATTRIBUTE_UV_0.at_shader_location(2),
                Mesh::ATTRIBUTE_TANGENT
                    .at_shader_location(3),
            ])?,
        ];
        // 不剔背面。立方体小到几乎不占片元,省下的那点填充率抵不上「绕序排错
        // 就整面消失」这种排查代价;而且散开的那几档预设里,从背后看见立方体
        // 的内壁反而是对的。
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

/// `cloud.wgsl` 在资产系统里的路径。着色器随二进制内嵌(`embedded_asset!`),
/// 应用无头运行、没有 `assets/` 目录。
const SHADER_PATH: &str = "embedded://render3d/cloud.wgsl";

/// 把顶点数据烘成一份 bevy `Mesh`。
///
/// 角偏移借法线位、面法线借切线位 —— 这两个属性槽本来就空着(点云不吃 bevy
/// 的光照,也不做法线贴图),借它们比注册两套自定义顶点属性少一整摊代码。
/// 代价是名字对不上语义,故 `cloud.wgsl` 的 `Vertex` 里逐字段注明了实际含义。
pub(crate) fn build_cloud_mesh(
    vertices: CloudVertices,
) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vertices.positions,
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vertices.corners,
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_UV_0,
        vertices.uvs,
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_TANGENT,
        vertices.tangents,
    );
    mesh.insert_indices(Indices::U32(vertices.indices));
    mesh
}

// ── 指针交互:涟漪与拖动旋转 ────────────────────────────────────────────

// 这里曾有一整套「指针涟漪」:鼠标经过的地方顶起一个高斯包,配一圈往外走的环。
// 它照抄自参照项目,而在本仓库里**从未真正生效过** —— uniform 的字段顺序与
// cloud.wgsl 写反了,着色器一直从对齐用的填充字节里读它的幅度,读到的是 0,

#[cfg(test)]
mod tests;
