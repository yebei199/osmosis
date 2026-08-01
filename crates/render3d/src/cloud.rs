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

/// 频谱行按低/中/高拆出的三段电平,各自归一到 0..=1。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Levels {
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
}

/// 把 512 字节的频谱行拆成三段电平(各段取均值 / 255)。
///
/// 段界与 `audio::spectrum` 的 bin 布局对齐:低频挤在行首。长度不足 512 视为
/// 坏载荷,给静音电平 —— 载荷长度是跨 crate 的外部输入,坏了不 panic。
pub(crate) fn band_levels(spectrum: &[u8]) -> Levels {
    if spectrum.len() < 512 {
        return Levels::default();
    }
    let average = |range: core::ops::Range<usize>| {
        let len = range.len() as f32;
        spectrum[range]
            .iter()
            .map(|v| f32::from(*v))
            .sum::<f32>()
            / (len * 255.0)
    };
    Levels {
        bass: average(0..32),
        mid: average(32..160),
        treble: average(160..512),
    }
}

/// 点云网格边长:每行每列各 384 颗,共 147 456 颗。
///
/// 曾经一路提到 384,追的是「格距小到眼睛自己会拼」—— 结果拼得太好了:那不是
/// 像素画,是一张打了网点的照片。像素画的重点恰恰在**格子看得见**,一格就是一个
/// 色块,而不是一个采样点。96 格是专辑封面做像素化处理时常用的量级,人脸还认得
/// 出轮廓,但一眼就知道它是由方块砌的。
///
/// 顺带把粒子数砍到 1/16(14.7 万 → 9216),这才养得起把每颗从方片换成立方体。
pub(crate) const CLOUD_GRID: usize = 96;

/// 点云平面在世界里的边长,同原版。
///
/// 曾经放大到 12 让它溢出视口,但铺满四边之后点云不再像「一张图」,只像一层贴在
/// 屏幕上的网点。收回原版这一档:画面中间一块有边界的封面,四周留给背景。
/// 圆点大小按格距走(见 [`CELL_WORLD`]),平面缩回来点也跟着变细。
const PLANE_SIZE: f32 = 4.8;

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
/// 看出体积与阶梯 —— 这才是「像素画」而不是「打了网点的照片」。
///
/// 每颗 24 个顶点(六面各四个,面不共享顶点,好让每个面有自己的平法线)、
/// 36 个索引。四个数组逐顶点等长;分成裸数组而不是「顶点结构体的数组」,
/// 是因为 bevy 的 `Mesh` 本就按属性分别收。
pub(crate) struct CloudVertices {
    /// 格点在点云平面上的基准位,z 恒为 0 —— 位移在顶点着色器里加。
    pub positions: Vec<[f32; 3]>,
    /// 封面纹理的采样坐标,同一颗粒子的 24 个顶点相同。
    pub uvs: Vec<[f32; 2]>,
    /// 立方体的角偏移,三个分量各取 ±1。乘上半边长就是这个角相对格心的位置。
    pub corners: Vec<[f32; 3]>,
    /// (面法线 x, y, z, 逐粒子随机数)。法线用来给面分明暗;随机数给每颗粒子
    /// 一份「个性」(相位、闪烁、散射方向都从它派生),同一颗的 24 个顶点相同。
    pub tangents: Vec<[f32; 4]>,
    /// 三角形索引,每颗粒子十二个三角形。
    pub indices: Vec<u32>,
}

/// 每颗粒子的顶点数:六个面各四个,面之间不共享 —— 共享了法线就得插值,
/// 立方体的棱会被抹圆,看着像颗骰子而不是方块。
pub(crate) const CUBE_VERTICES: usize = 24;

/// 每颗粒子的索引数:十二个三角形。
pub(crate) const CUBE_INDICES: usize = 36;

/// 立方体的六个面:(朝外的面法线, 该面四个角的偏移)。
///
/// 角按面内逆时针排(从立方体外面看),索引取 0-1-2 / 0-2-3。背面剔除在
/// [`CloudMaterial::specialize`] 里关掉了,所以绕序错了也不会整面消失 ——
/// 但排对了才不用靠那个兜底。
const CUBE_FACES: [([f32; 3], [[f32; 3]; 4]); 6] = [
    // +X
    (
        [1.0, 0.0, 0.0],
        [
            [1.0, -1.0, 1.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [1.0, 1.0, 1.0],
        ],
    ),
    // -X
    (
        [-1.0, 0.0, 0.0],
        [
            [-1.0, -1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [-1.0, 1.0, 1.0],
            [-1.0, 1.0, -1.0],
        ],
    ),
    // +Y(顶面,最亮的那一面)
    (
        [0.0, 1.0, 0.0],
        [
            [-1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
            [1.0, 1.0, -1.0],
            [-1.0, 1.0, -1.0],
        ],
    ),
    // -Y
    (
        [0.0, -1.0, 0.0],
        [
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, -1.0, 1.0],
            [-1.0, -1.0, 1.0],
        ],
    ),
    // +Z(正对镜头的那一面)
    (
        [0.0, 0.0, 1.0],
        [
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
        ],
    ),
    // -Z
    (
        [0.0, 0.0, -1.0],
        [
            [1.0, -1.0, -1.0],
            [-1.0, -1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [1.0, 1.0, -1.0],
        ],
    ),
];

/// 烘出点云 mesh 的顶点数据。建一次就不动,之后每帧只更新 uniform。
pub(crate) fn cloud_vertices() -> CloudVertices {
    let particles = CLOUD_GRID * CLOUD_GRID;
    let mut positions =
        Vec::with_capacity(particles * CUBE_VERTICES);
    let mut uvs =
        Vec::with_capacity(particles * CUBE_VERTICES);
    let mut corners =
        Vec::with_capacity(particles * CUBE_VERTICES);
    let mut tangents =
        Vec::with_capacity(particles * CUBE_VERTICES);
    let mut indices =
        Vec::with_capacity(particles * CUBE_INDICES);

    let grid = CLOUD_GRID as f32;
    let texel = 1.0 / grid;

    for i in 0..particles {
        let gx = (i % CLOUD_GRID) as f32;
        let gy = (i / CLOUD_GRID) as f32;
        // 取格心而不是格点边缘:贴边会采到相邻像素的插值,点云边缘会糊出
        // 一圈不属于封面的颜色。
        //
        // v 要翻:格点的 y 越大越靠画面**上**方,而纹理的 v 越大越靠图的**下**边。
        // 不翻的话封面在点云里是倒的 —— 桌面上一眼看不出(很多封面上下差别不大),
        // 但天空在下、地面在上的那些一翻就穿帮。
        let uv =
            [(gx + 0.5) * texel, 1.0 - (gy + 0.5) * texel];
        let base = [
            (gx / (grid - 1.0) - 0.5) * PLANE_SIZE,
            (gy / (grid - 1.0) - 0.5) * PLANE_SIZE,
            0.0,
        ];
        let rand = hash01(i);

        for (normal, face) in CUBE_FACES {
            let first = u32::try_from(positions.len())
                .expect("点云顶点数远小于 u32 上限");
            for corner in face {
                positions.push(base);
                uvs.push(uv);
                corners.push(corner);
                tangents.push([
                    normal[0], normal[1], normal[2], rand,
                ]);
            }
            indices.extend_from_slice(&[
                first,
                first + 1,
                first + 2,
                first,
                first + 2,
                first + 3,
            ]);
        }
    }

    CloudVertices {
        positions,
        uvs,
        corners,
        tangents,
        indices,
    }
}

/// 下标 → [0,1) 的确定性散列(乘法混淆 + 移位)。
///
/// 不引 rand:顶点数据烘一次就固定,但「同一颗粒子每次构建拿到同一个随机数」
/// 让点云在重建前后长得一样,调参时对得上。
fn hash01(i: usize) -> f32 {
    let mut x = i as u32;
    x = x.wrapping_mul(0xA076_1D65);
    x ^= x >> 16;
    x = x.wrapping_mul(0x8EBC_6AF1);
    x ^= x >> 13;
    (x & 0x00FF_FFFF) as f32 / 16_777_216.0
}

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
// 而最后一步是乘上那个幅度。顺序对齐之后它第一次露面:一个跟着鼠标走的巨大
// 空洞,把整片点云推开。没人要过这个效果,遂整条删除,不留开关。
//
// 拖动旋转(下面的 Spin)是另一回事,留着 —— 那个一直都在工作。

/// 横拖一像素转多少弧度(绕 Y),同原版 `PARTICLE_POINTER_SPIN_Y`。
const SPIN_PER_PIXEL_Y: f32 = 0.0034;

/// 纵拖一像素转多少弧度(绕 X),同原版 `PARTICLE_POINTER_SPIN_X`。
const SPIN_PER_PIXEL_X: f32 = 0.0032;

/// 角速度上限,rad/s。同原版 `PARTICLE_SPIN_MAX`。
const SPIN_MAX: f32 = 6.2;

/// 松手时把「这一段拖了多少」换算成角速度的系数,同原版 `applyParticleSpinDrag`。
const SPIN_RELEASE: f32 = 0.46;

/// 每帧留下多少角速度,同原版 `particleSpin.damping`。
const SPIN_DAMPING: f32 = 0.90;

/// 拖动带来的点云自转,外加松手后的惯性。
///
/// 转的是**点云自己**而不是相机 —— 原版就是把 `gestureRotation` 加到
/// `particles.rotation` 上。相机一动,遮挡层那台就得跟着动,两层还得逐像素对齐;
/// 转物体没有这个牵连。
#[derive(Default)]
pub(crate) struct Spin {
    /// 累计角度,弧度。
    pitch: f32,
    yaw: f32,
    /// 松手后的角速度,rad/s。
    pitch_rate: f32,
    yaw_rate: f32,
}

impl Spin {
    /// 拖了 `(dx, dy)` 个像素,`dt` 秒。横向转 yaw、纵向转 pitch。
    pub(crate) fn drag(
        &mut self,
        dx: f32,
        dy: f32,
        dt: f32,
    ) {
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        let d_pitch = dy * SPIN_PER_PIXEL_X;
        let d_yaw = dx * SPIN_PER_PIXEL_Y;
        self.pitch += d_pitch;
        self.yaw += d_yaw;
        if dt.is_finite() && dt > 0.0 {
            self.pitch_rate =
                clamp_rate(d_pitch / dt * SPIN_RELEASE);
            self.yaw_rate =
                clamp_rate(d_yaw / dt * SPIN_RELEASE);
        }
    }

    /// 松手后的惯性:按角速度继续转,同时衰减。坏 `dt` 当作没走。
    pub(crate) fn coast(&mut self, dt: f32) {
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        self.pitch += self.pitch_rate * dt;
        self.yaw += self.yaw_rate * dt;
        // 原版按帧衰减 0.90;这里按时间衰减,帧率一变角速度的衰减快慢才不跟着变。
        let decay = SPIN_DAMPING.powf(dt * 60.0);
        self.pitch_rate *= decay;
        self.yaw_rate *= decay;
        // 衰到看不出来就归零,免得留一份永远非零的状态每帧标脏。
        if self.pitch_rate.abs() < 1e-4 {
            self.pitch_rate = 0.0;
        }
        if self.yaw_rate.abs() < 1e-4 {
            self.yaw_rate = 0.0;
        }
    }

    /// 当前的累计角度 (pitch, yaw),弧度。
    pub(crate) fn angles(&self) -> (f32, f32) {
        (self.pitch, self.yaw)
    }
}

/// 角速度钳进 ±[`SPIN_MAX`];非有限值归零,同原版 `clampParticleSpinVelocity`。
fn clamp_rate(rate: f32) -> f32 {
    if rate.is_finite() {
        rate.clamp(-SPIN_MAX, SPIN_MAX)
    } else {
        0.0
    }
}

/// 颜色从旧封面走到新封面要多久,秒。
const COLOR_FADE_SECS: f32 = 0.9;

/// 换歌脉冲从满到零要多久,秒。比颜色渐变短:脉冲是「一下」,渐变是「一段」。
const BURST_SECS: f32 = 0.55;

/// 换歌后的过渡:同一个计时器派生出「颜色渐变」与「burst 脉冲」两条曲线。
///
/// 原版是两个各自衰减的 uniform(`uColorMixT` / `uBurstAmt`),这里合成一个 ——
/// 它们永远同时开始,分开存只是多一个会走散的状态。
pub(crate) struct TrackTransition {
    /// 距上次换歌的秒数。初值取一个已经跑完的值:首曲没有「上一首」可渐变。
    elapsed: f32,
}

/// 初始状态 = 过渡早已结束:`color_mix` 为 1、`burst` 为 0。
impl Default for TrackTransition {
    fn default() -> Self {
        Self {
            elapsed: COLOR_FADE_SECS,
        }
    }
}

impl TrackTransition {
    /// 换歌:计时归零,旧封面开始向新封面过渡,同时起一发 burst。
    pub(crate) fn start(&mut self) {
        self.elapsed = 0.0;
    }

    /// 按播放页时钟推进 `dt` 秒。
    ///
    /// `dt` 是两帧时间戳相减来的,非有限或为负时当作没走 —— 时钟可以被门冻结、
    /// 可以被系统改,但过渡不能因此倒着走。计满即停,不让计数器一直涨。
    pub(crate) fn advance(&mut self, dt: f32) {
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        self.elapsed =
            (self.elapsed + dt).min(COLOR_FADE_SECS);
    }

    /// 新旧封面的混合进度:0 = 全旧,1 = 全新。
    pub(crate) fn color_mix(&self) -> f32 {
        (self.elapsed / COLOR_FADE_SECS).clamp(0.0, 1.0)
    }

    /// burst 脉冲强度:换歌瞬间 1,随后衰减到 0。
    ///
    /// 平方衰减而不是线性 —— 线性的尾巴拖着不走,观感上像粒子回不到位。
    pub(crate) fn burst(&self) -> f32 {
        let left = (1.0 - self.elapsed / BURST_SECS)
            .clamp(0.0, 1.0);
        left * left
    }
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    // ── 频段拆分(沿用尘埃层那一版,行为未变)────────────────────────────

    /// 频段拆分:低段全 255、中段全 128、高段全 0 的合成行,三段均值各归其位。
    #[test]
    fn band_levels_average_the_three_ranges() {
        let mut spectrum = [0u8; 512];
        spectrum[..32].fill(255);
        spectrum[32..160].fill(128);
        let l = band_levels(&spectrum);
        assert!(
            (l.bass - 1.0).abs() < 1e-3,
            "低段 {}",
            l.bass
        );
        assert!(
            (l.mid - 128.0 / 255.0).abs() < 1e-3,
            "中段 {}",
            l.mid
        );
        assert_eq!(l.treble, 0.0);
    }

    /// 频谱行长度不足(空/短):三段全给 0,不 panic —— 载荷长度是外部输入。
    #[test]
    fn short_spectrum_yields_zero_levels_not_panic() {
        assert_eq!(band_levels(&[]), Levels::default());
        assert_eq!(
            band_levels(&[255u8; 10]),
            Levels::default()
        );
    }

    // ── 点云网格 ──────────────────────────────────────────────────────

    /// 每颗粒子出 24 个顶点、36 个索引(六面各四顶点两三角),总数对得上网格。
    #[test]
    fn every_particle_gets_one_cube() {
        let v = cloud_vertices();
        let particles = CLOUD_GRID * CLOUD_GRID;
        assert_eq!(
            v.positions.len(),
            particles * CUBE_VERTICES
        );
        assert_eq!(v.uvs.len(), particles * CUBE_VERTICES);
        assert_eq!(
            v.corners.len(),
            particles * CUBE_VERTICES
        );
        assert_eq!(
            v.tangents.len(),
            particles * CUBE_VERTICES
        );
        assert_eq!(
            v.indices.len(),
            particles * CUBE_INDICES
        );
    }

    /// 六个面各自带一个朝外的轴向法线,同一个面的四个顶点法线相同 ——
    /// 面内不一致就得插值,立方体的棱会被抹圆,看着像颗骰子而不是方块。
    #[test]
    fn each_cube_face_carries_one_outward_normal() {
        let v = cloud_vertices();
        let mut seen = Vec::new();
        for face in 0..6 {
            let base = face * 4;
            let normal = v.tangents[base];
            for offset in 1..4 {
                assert_eq!(
                    normal[..3],
                    v.tangents[base + offset][..3],
                    "面 {face} 的法线不一致"
                );
            }
            // 轴向单位向量:三个分量里恰好一个是 ±1,其余为 0。
            let magnitude: f32 =
                normal[..3].iter().map(|c| c.abs()).sum();
            assert_eq!(
                magnitude, 1.0,
                "面 {face} 的法线不是轴向单位向量: {normal:?}"
            );
            seen.push([
                normal[0] as i32,
                normal[1] as i32,
                normal[2] as i32,
            ]);
        }
        seen.sort_unstable();
        assert_eq!(
            seen,
            vec![
                [-1, 0, 0],
                [0, -1, 0],
                [0, 0, -1],
                [0, 0, 1],
                [0, 1, 0],
                [1, 0, 0],
            ],
            "六个面没有覆盖全部朝向"
        );
    }

    /// 采样 uv 落在纹理内部(取格心 (g+0.5)/grid,不贴边)—— 贴边会采到
    /// 相邻像素的插值,点云边缘会糊出一圈不属于封面的颜色。
    #[test]
    fn cover_uvs_stay_inside_the_texture() {
        let v = cloud_vertices();
        // 边界那一格恰好落在 margin 上,而 v 是 `1 - g·texel` 减出来的 ——
        // 减法丢掉几位精度,断言得留容差,否则测的是浮点而不是行为。
        let margin = 0.5 / CLOUD_GRID as f32 - f32::EPSILON;
        for uv in &v.uvs {
            for c in uv {
                assert!(
                    *c >= margin && *c <= 1.0 - margin,
                    "采样坐标贴边: {uv:?}"
                );
            }
        }
    }

    /// 封面在点云里不能是倒的:格点 y 越大越靠画面上方,纹理 v 越大越靠图的下边,
    /// 两者反向,所以 v 必须翻。这条钉的是那一次翻转。
    #[test]
    fn the_cover_is_not_upside_down() {
        let v = cloud_vertices();
        // 第一行格点(gy=0)在画面最下方,该采到封面的**下**边(v 接近 1)。
        assert!(
            v.uvs[0][1] > 0.99,
            "最下面一行采到了封面顶部: {:?}",
            v.uvs[0]
        );
        // 最后一行格点在画面最上方,该采到封面的上边(v 接近 0)。
        let top =
            (CLOUD_GRID * CLOUD_GRID - 1) * CUBE_VERTICES;
        assert!(
            v.uvs[top][1] < 0.01,
            "最上面一行采到了封面底部: {:?}",
            v.uvs[top]
        );
    }

    /// 同一颗粒子的 24 个顶点共享同一个采样 uv、同一个随机数、同一个基准位 ——
    /// 否则一颗粒子的各个角会被算到不同位置,立方体会被撕开。
    #[test]
    fn one_particle_shares_its_uv_and_random() {
        let v = cloud_vertices();
        for cube in 0..CLOUD_GRID * CLOUD_GRID {
            let base = cube * CUBE_VERTICES;
            for offset in 1..CUBE_VERTICES {
                assert_eq!(
                    v.uvs[base],
                    v.uvs[base + offset],
                    "粒子 {cube} 的顶点 {offset} uv 不一致"
                );
                assert_eq!(
                    v.tangents[base][3],
                    v.tangents[base + offset][3],
                    "粒子 {cube} 的顶点 {offset} 随机数不一致"
                );
                assert_eq!(
                    v.positions[base],
                    v.positions[base + offset],
                    "粒子 {cube} 的顶点 {offset} 基准位不一致"
                );
            }
        }
    }

    /// 角偏移覆盖立方体的八个角(±1, ±1, ±1),缺一个就少一块。
    ///
    /// 顶点是 24 个而不是 8 个(面不共享),所以每个角出现三次 —— 去重之后
    /// 才是八个。
    #[test]
    fn corner_offsets_cover_the_cube() {
        let v = cloud_vertices();
        for cube in 0..CLOUD_GRID * CLOUD_GRID {
            let base = cube * CUBE_VERTICES;
            let mut seen: Vec<_> = v.corners
                [base..base + CUBE_VERTICES]
                .iter()
                .map(|c| {
                    (c[0] as i32, c[1] as i32, c[2] as i32)
                })
                .collect();
            seen.sort_unstable();
            let corners_per_cube = seen.len();
            seen.dedup();
            assert_eq!(
                seen.len(),
                8,
                "粒子 {cube} 的八角不全"
            );
            assert_eq!(
                corners_per_cube, CUBE_VERTICES,
                "粒子 {cube} 的顶点数不对"
            );
            for corner in seen {
                assert_eq!(
                    (
                        corner.0.abs(),
                        corner.1.abs(),
                        corner.2.abs()
                    ),
                    (1, 1, 1),
                    "粒子 {cube} 有个角不在 ±1 上: {corner:?}"
                );
            }
        }
    }

    /// 基准位铺满点云平面且共面(z=0):x/y 覆盖 ±PLANE_SIZE/2 且不越界。
    #[test]
    fn base_positions_fill_the_plane() {
        let v = cloud_vertices();
        let half = PLANE_SIZE / 2.0;
        let (mut min_x, mut max_x) = (f32::MAX, f32::MIN);
        let (mut min_y, mut max_y) = (f32::MAX, f32::MIN);
        for p in &v.positions {
            assert_eq!(p[2], 0.0, "基准位不共面: {p:?}");
            assert!(
                p[0].abs() <= half + 1e-4
                    && p[1].abs() <= half + 1e-4,
                "基准位出界: {p:?}"
            );
            min_x = min_x.min(p[0]);
            max_x = max_x.max(p[0]);
            min_y = min_y.min(p[1]);
            max_y = max_y.max(p[1]);
        }
        // 首尾格点正好落在平面两端,铺满而不是缩在中间。
        assert!(
            (min_x + half).abs() < 1e-4,
            "左边没铺到: {min_x}"
        );
        assert!(
            (max_x - half).abs() < 1e-4,
            "右边没铺到: {max_x}"
        );
        assert!(
            (min_y + half).abs() < 1e-4,
            "下边没铺到: {min_y}"
        );
        assert!(
            (max_y - half).abs() < 1e-4,
            "上边没铺到: {max_y}"
        );
    }

    // ── 视觉预设 ──────────────────────────────────────────────────────

    /// 每一档都保住自己的下标 —— 加档时着色器按下标分支,错一位就画成另一个预设。
    /// 现在只有一档,这条守的是"默认档的下标必须是 0"。
    #[test]
    fn every_preset_keeps_its_index() {
        for i in 0..PRESET_COUNT {
            assert_eq!(
                preset_index(i),
                i as u32,
                "第 {i} 档下标被改了"
            );
        }
    }

    /// 越界的编号回默认档 —— 编号来自 `.slint`,是跨层的外部输入,
    /// 给个不存在的档不该画出一片空白。
    #[test]
    fn an_out_of_range_preset_falls_back_to_the_default() {
        for i in [-1, -100, PRESET_COUNT, PRESET_COUNT + 7]
        {
            assert_eq!(
                preset_index(i),
                0,
                "编号 {i} 该回默认档"
            );
        }
    }

    // ── 窄视口 ────────────────────────────────────────────────────────

    /// 横屏与方屏不收:物体类预设按相机距离取景,横向够宽就不必动。
    #[test]
    fn wide_viewports_keep_the_base_object_scale() {
        for (w, h) in [(1920u32, 1080u32), (1080, 1080)] {
            assert!(
                (object_scale(w, h) - OBJECT_SCALE).abs()
                    < 1e-6,
                "{w}x{h} 不该收缩"
            );
        }
    }

    /// 竖屏按长宽比收:小米13 竖屏 aspect 0.45,球体不收就左右出画(真机实测)。
    #[test]
    fn portrait_viewports_shrink_by_the_aspect_ratio() {
        let scale = object_scale(1080, 2400);
        let aspect = 1080.0 / 2400.0;
        assert!(
            (scale - OBJECT_SCALE * aspect).abs() < 1e-6,
            "竖屏收缩量不对: {scale}"
        );
        assert!(scale < OBJECT_SCALE, "竖屏没收");
    }

    /// 0 尺寸(首帧/刚重建)退回基准倍数,不除零、不把物体缩没。
    #[test]
    fn a_zero_sized_viewport_falls_back_to_the_base_scale()
    {
        for (w, h) in [(0u32, 1080u32), (1080, 0), (0, 0)] {
            assert_eq!(object_scale(w, h), OBJECT_SCALE);
        }
    }

    // ── 拖动旋转 ──────────────────────────────────────────────────────

    /// 横拖绕 Y、纵拖绕 X,系数照源码。
    #[test]
    fn dragging_accumulates_rotation_on_both_axes() {
        let mut spin = Spin::default();
        spin.drag(100.0, 0.0, 1.0 / 60.0);
        let (pitch, yaw) = spin.angles();
        assert_eq!(pitch, 0.0, "横拖不该改 pitch");
        assert!(
            (yaw - 100.0 * SPIN_PER_PIXEL_Y).abs() < 1e-6,
            "横拖的 yaw 不对: {yaw}"
        );

        spin.drag(0.0, 50.0, 1.0 / 60.0);
        let (pitch, _) = spin.angles();
        assert!(
            (pitch - 50.0 * SPIN_PER_PIXEL_X).abs() < 1e-6,
            "纵拖的 pitch 不对: {pitch}"
        );
    }

    /// 松手后按惯性继续转,角速度单调衰减到 0 且不反弹。
    #[test]
    fn releasing_keeps_spinning_and_decays_to_rest() {
        let mut spin = Spin::default();
        spin.drag(100.0, 0.0, 1.0 / 60.0);
        let (_, after_drag) = spin.angles();

        spin.coast(1.0 / 60.0);
        let (_, moved) = spin.angles();
        assert!(moved > after_drag, "松手后没有继续转");

        let mut last = moved;
        for _ in 0..600 {
            spin.coast(1.0 / 60.0);
            let (_, now) = spin.angles();
            assert!(
                now >= last,
                "转回去了: {last} -> {now}"
            );
            last = now;
        }
        // 衰减完之后再转也不动了。
        let before = last;
        spin.coast(1.0 / 60.0);
        assert_eq!(
            spin.angles().1,
            before,
            "角速度没有衰减到零"
        );
    }

    /// 甩得再快角速度也压在上限 —— 一帧内的极小 `dt` 会把速度算上天。
    #[test]
    fn spin_velocity_is_clamped() {
        let mut spin = Spin::default();
        spin.drag(10_000.0, 10_000.0, 1e-6);
        // 一帧惯性最多推进 SPIN_MAX * dt。
        let step = 1.0 / 60.0;
        let before = spin.angles();
        spin.coast(step);
        let after = spin.angles();
        assert!(
            (after.0 - before.0).abs()
                <= SPIN_MAX * step + 1e-6,
            "pitch 角速度越界"
        );
        assert!(
            (after.1 - before.1).abs()
                <= SPIN_MAX * step + 1e-6,
            "yaw 角速度越界"
        );
    }

    /// 坏 `dt` / 坏位移都不推进也不 panic。
    #[test]
    fn bad_delta_time_leaves_the_spin_alone() {
        let mut spin = Spin::default();
        spin.drag(f32::NAN, 1.0, 1.0 / 60.0);
        assert_eq!(
            spin.angles(),
            (0.0, 0.0),
            "坏位移进了累计角度"
        );

        spin.drag(100.0, 0.0, 1.0 / 60.0);
        let before = spin.angles();
        for dt in [f32::NAN, -1.0, f32::INFINITY, 0.0] {
            spin.coast(dt);
        }
        assert_eq!(
            spin.angles(),
            before,
            "坏 dt 推进了惯性"
        );
    }

    // ── 换歌过渡 ──────────────────────────────────────────────────────

    /// 从未换过歌:颜色全给新封面、没有脉冲 —— 首曲没有「上一首」可渐变,
    /// 起手就渐变会让第一首歌从一片占位色淡入。
    #[test]
    fn fresh_transition_shows_only_the_new_cover() {
        let t = TrackTransition::default();
        assert_eq!(t.color_mix(), 1.0);
        assert_eq!(t.burst(), 0.0);
    }

    /// 颜色渐变:换歌归零后单调走向 1,到头钳死不越界。
    #[test]
    fn color_mix_walks_from_old_to_new_and_stops_at_one() {
        let mut t = TrackTransition::default();
        t.start();
        assert_eq!(t.color_mix(), 0.0);

        let mut last = 0.0;
        for _ in 0..30 {
            t.advance(0.05);
            let now = t.color_mix();
            assert!(
                now >= last,
                "颜色混合倒退: {last} -> {now}"
            );
            assert!(
                (0.0..=1.0).contains(&now),
                "颜色混合越界: {now}"
            );
            last = now;
        }
        assert_eq!(last, 1.0, "推够时间后该完全是新封面");
    }

    /// burst:换歌瞬间满,单调衰减到 0 之后不反弹。
    #[test]
    fn burst_decays_to_zero_and_stays_there() {
        let mut t = TrackTransition::default();
        t.start();
        assert_eq!(t.burst(), 1.0);

        let mut last = 1.0;
        for _ in 0..30 {
            t.advance(0.05);
            let now = t.burst();
            assert!(
                now <= last,
                "脉冲反弹: {last} -> {now}"
            );
            assert!(
                (0.0..=1.0).contains(&now),
                "脉冲越界: {now}"
            );
            last = now;
        }
        assert_eq!(last, 0.0, "推够时间后脉冲该归零");
    }

    /// 坏的 `dt`(NaN / 负数 / 无穷)不推进也不 panic,两条曲线仍然有界 ——
    /// `dt` 来自两帧时间戳相减,时钟会被门冻结、也会被系统改。
    #[test]
    fn bad_delta_time_keeps_the_transition_bounded() {
        let mut t = TrackTransition::default();
        t.start();
        for dt in [f32::NAN, -1.0, f32::INFINITY, -0.0] {
            t.advance(dt);
            assert_eq!(
                t.color_mix(),
                0.0,
                "坏 dt {dt} 推进了过渡"
            );
            assert_eq!(t.burst(), 1.0);
        }
    }

    /// 索引全部指向存在的顶点 —— 越界索引在 GPU 上是未定义行为,不是报错。
    #[test]
    fn indices_reference_existing_vertices() {
        let v = cloud_vertices();
        let count = v.positions.len() as u32;
        for i in &v.indices {
            assert!(
                *i < count,
                "索引 {i} 越界(共 {count} 个顶点)"
            );
        }
    }
}
