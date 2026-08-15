//! 点云的纯计算:频谱行拆成三段电平,以及每颗粒子那个立方体的顶点数据。
//!
//! 两者都不碰 GPU 资源,只出数组;上传交给 [`super::build_cloud_mesh`]。

use super::{CLOUD_GRID, PLANE_SIZE};

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

#[cfg(test)]
mod tests;
