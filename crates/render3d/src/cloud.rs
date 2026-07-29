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

/// 点云网格边长:每行每列各 183 颗,共 33 489 颗。
///
/// 原版 `coverParticleGridForResolution` 在默认档 `coverResolution = 1.55` 下的
/// 结果(`round(118 × 1.55) = 183`,钳进 88..=183 后取奇数)。没有设置面板,
/// 先钉成常数;真机发热读数出来后可能整体下调(见 `docs/adr/0012`)。
pub(crate) const CLOUD_GRID: usize = 183;

/// 点云平面在世界里的边长。
///
/// 原版是 4.8,在我们的相机下那正好是一块看得见四条边的方板。这里放大到溢出视口,
/// 边界跑到画外去 —— 观感上是「封面炸成一片」而不是「一块点阵牌子」。
/// 放大后格点间距同比变大,故 [`POINT_SIZE`] 也按同一倍数放大,点与缝的比例不变。
const PLANE_SIZE: f32 = 12.0;

/// 一份点云 mesh 的顶点数据:每颗粒子一个朝向相机的小四边形。
///
/// 四个数组等长(`positions` / `uvs` / `corners` 逐顶点),`indices` 每颗六个。
/// 分成裸数组而不是「顶点结构体的数组」,是因为 bevy 的 `Mesh` 本就按属性分别收。
pub(crate) struct CloudVertices {
    /// 格点在点云平面上的基准位,z 恒为 0 —— 位移在顶点着色器里加。
    pub positions: Vec<[f32; 3]>,
    /// 封面纹理的采样坐标,同一颗粒子的四个角相同。
    pub uvs: Vec<[f32; 2]>,
    /// (四边形角偏移 x, 角偏移 y, 逐粒子随机数)。前两个分量把点扩成方片,
    /// 第三个给每颗粒子一份「个性」(相位、闪烁、散射方向都从它派生)。
    pub corners: Vec<[f32; 3]>,
    /// 三角形索引,每颗粒子两个三角形。
    pub indices: Vec<u32>,
}

/// 烘出点云 mesh 的顶点数据。建一次就不动,之后每帧只更新 uniform。
pub(crate) fn cloud_vertices() -> CloudVertices {
    let particles = CLOUD_GRID * CLOUD_GRID;
    let mut positions = Vec::with_capacity(particles * 4);
    let mut uvs = Vec::with_capacity(particles * 4);
    let mut corners = Vec::with_capacity(particles * 4);
    let mut indices = Vec::with_capacity(particles * 6);

    let grid = CLOUD_GRID as f32;
    let texel = 1.0 / grid;
    // 四边形的四个角,顺序与下面的索引对应(左下、右下、左上、右上)。
    const QUAD: [[f32; 2]; 4] = [
        [-1.0, -1.0],
        [1.0, -1.0],
        [-1.0, 1.0],
        [1.0, 1.0],
    ];

    for i in 0..particles {
        let gx = (i % CLOUD_GRID) as f32;
        let gy = (i / CLOUD_GRID) as f32;
        // 取格心而不是格点边缘:贴边会采到相邻像素的插值,点云边缘会糊出
        // 一圈不属于封面的颜色。
        let uv = [(gx + 0.5) * texel, (gy + 0.5) * texel];
        let base = [
            (gx / (grid - 1.0) - 0.5) * PLANE_SIZE,
            (gy / (grid - 1.0) - 0.5) * PLANE_SIZE,
            0.0,
        ];
        let rand = hash01(i);

        let first = u32::try_from(positions.len())
            .expect("点云顶点数远小于 u32 上限");
        for corner in QUAD {
            positions.push(base);
            uvs.push(uv);
            corners.push([corner[0], corner[1], rand]);
        }
        indices.extend_from_slice(&[
            first,
            first + 1,
            first + 2,
            first + 2,
            first + 1,
            first + 3,
        ]);
    }

    CloudVertices {
        positions,
        uvs,
        corners,
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

/// 圆点半径,单位是渲染目标的像素。
///
/// 原版按纵深缩放并钳进 1.05..4.95,这里取定值 —— 少一个每帧要算的量,
/// 观感差别肉眼看不出。数值是原版那一段的中位数 2.4 乘上 [`PLANE_SIZE`] 的放大倍数,
/// 于是点与缝的比例与原版一致。
const POINT_SIZE: f32 = 6.0;

/// 点云材质的 uniform 块,逐字段镜像 `cloud.wgsl` 的 `CloudParams`。
#[derive(Clone, Copy, ShaderType)]
pub(crate) struct CloudParams {
    pub time: f32,
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
    pub intensity: f32,
    pub has_cover: f32,
    pub alpha_cutoff: f32,
    pub point_size: f32,
}

/// 静止、无封面的一帧参数:常量项按原版默认档,音频项全零。
impl Default for CloudParams {
    fn default() -> Self {
        Self {
            time: 0.0,
            bass: 0.0,
            mid: 0.0,
            treble: 0.0,
            // 原版 uIntensity 的默认值。
            intensity: 0.85,
            has_cover: 0.0,
            alpha_cutoff: ALPHA_CUTOFF,
            point_size: POINT_SIZE,
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
}

impl Material for CloudMaterial {
    fn vertex_shader() -> ShaderRef {
        SHADER_PATH.into()
    }

    fn fragment_shader() -> ShaderRef {
        SHADER_PATH.into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        #[cfg(target_os = "android")]
        return AlphaMode::Mask(ALPHA_CUTOFF);
        #[cfg(not(target_os = "android"))]
        return AlphaMode::Blend;
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
            ])?,
        ];
        Ok(())
    }
}

/// `cloud.wgsl` 在资产系统里的路径。着色器随二进制内嵌(`embedded_asset!`),
/// 应用无头运行、没有 `assets/` 目录。
const SHADER_PATH: &str = "embedded://render3d/cloud.wgsl";

/// 把顶点数据烘成一份 bevy `Mesh`。角偏移借用法线属性位 —— 点云不做光照,
/// 法线本来就空着,借它比自定义顶点属性少一整套注册。
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
    mesh.insert_indices(Indices::U32(vertices.indices));
    mesh
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

    /// 每颗粒子出 4 个顶点、6 个索引(两个三角形拼一个四边形),总数对得上网格。
    #[test]
    fn every_particle_gets_one_quad() {
        let v = cloud_vertices();
        let particles = CLOUD_GRID * CLOUD_GRID;
        assert_eq!(v.positions.len(), particles * 4);
        assert_eq!(v.uvs.len(), particles * 4);
        assert_eq!(v.corners.len(), particles * 4);
        assert_eq!(v.indices.len(), particles * 6);
    }

    /// 采样 uv 落在纹理内部(取格心 (g+0.5)/grid,不贴边)—— 贴边会采到
    /// 相邻像素的插值,点云边缘会糊出一圈不属于封面的颜色。
    #[test]
    fn cover_uvs_stay_inside_the_texture() {
        let v = cloud_vertices();
        let margin = 0.5 / CLOUD_GRID as f32;
        for uv in &v.uvs {
            for c in uv {
                assert!(
                    *c >= margin && *c <= 1.0 - margin,
                    "采样坐标贴边: {uv:?}"
                );
            }
        }
    }

    /// 同一颗粒子的 4 个顶点共享同一个采样 uv 与同一个随机数 —— 否则一颗粒子的
    /// 四个角会被算到不同位置,四边形会被撕开。
    #[test]
    fn one_particle_shares_its_uv_and_random() {
        let v = cloud_vertices();
        for quad in 0..CLOUD_GRID * CLOUD_GRID {
            let base = quad * 4;
            for offset in 1..4 {
                assert_eq!(
                    v.uvs[base],
                    v.uvs[base + offset],
                    "粒子 {quad} 的角 {offset} uv 不一致"
                );
                assert_eq!(
                    v.corners[base][2],
                    v.corners[base + offset][2],
                    "粒子 {quad} 的角 {offset} 随机数不一致"
                );
                assert_eq!(
                    v.positions[base],
                    v.positions[base + offset],
                    "粒子 {quad} 的角 {offset} 基准位不一致"
                );
            }
        }
    }

    /// 四个角偏移正好覆盖四边形的四角(±1, ±1),缺一个角就画不出方片。
    #[test]
    fn corner_offsets_cover_the_quad() {
        let v = cloud_vertices();
        for quad in 0..CLOUD_GRID * CLOUD_GRID {
            let mut seen: Vec<_> = v.corners
                [quad * 4..quad * 4 + 4]
                .iter()
                .map(|c| (c[0] as i32, c[1] as i32))
                .collect();
            seen.sort_unstable();
            assert_eq!(
                seen,
                vec![(-1, -1), (-1, 1), (1, -1), (1, 1)],
                "粒子 {quad} 的四角不全"
            );
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
