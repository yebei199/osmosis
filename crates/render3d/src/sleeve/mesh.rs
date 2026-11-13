//! 封面实物的几何:一块圆角、半圆倒角的薄封套,它关于地面的镜像,以及承它的那块地面。
//!
//! 全在「台面单位」里:封套正面是边长 1 的正方形,中心在原点,正面朝 +z。整座台面
//! 随视口缩放(`motion::stage_scale`),几何本身建一次就不动。纯数组,不碰 GPU;
//! 上传交给 [`super::build_mesh`]。

use std::f32::consts::{FRAC_PI_2, PI};

/// 一份网格的顶点数据,按属性分开的裸数组(bevy 的 `Mesh` 本就按属性分别收)。
pub(crate) struct Vertices {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// 封面的平面投影坐标:正面与倒角都按 xy 取,封面于是一直印到倒角上 ——
    /// 真封套的印刷是包边的,倒角处突然换成纸边颜色反而像一张贴纸。
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

/// 封套正面的半边长。
pub(crate) const HALF: f32 = 0.5;

/// 封套的厚度,以边长为单位。
///
/// 真的唱片封套约 3~4mm 对 31cm,这里取 3.4%:正对着看只是一圈细边,
/// 一倾斜就看得出它是个有厚度的东西 —— 正是「实物」两个字要的那一点。
pub(crate) const THICKNESS: f32 = 0.034;

/// 轮廓(从正面看的外缘)的圆角半径。纸板封套的角几乎是直的,只留一点点钝。
const CORNER: f32 = 0.022;

/// 每个圆角分几段。角很小,6 段已经看不出折线。
const CORNER_SEGMENTS: usize = 6;

/// 半圆倒角分几段。倒角承接边缘光(随低频亮起),段数少了那圈光会一节一节的。
const BEVEL_SEGMENTS: usize = 8;

/// 地面在台面单位里的范围:横向半宽、靠相机那一沿、远处那一沿。
///
/// 往远处铺得比往近处深得多,不是为了画面 —— 远处早就淡到透明了 —— 而是为了
/// 半透明排序:bevy 按网格包围盒中心的远近从后往前画,地面的中心得明显落在封套
/// (以及换歌时退场的那一张)后面,倒影才会画在地面之上而不是被地面盖住。
pub(crate) const FLOOR_HALF_WIDTH: f32 = 2.4;
pub(crate) const FLOOR_NEAR: f32 = 1.6;
pub(crate) const FLOOR_FAR: f32 = -5.0;

// 地面的包围盒中心要明显落在封套(以及退场时退到 z ≈ −0.6 的那一张)后面,
// 否则半透明排序会在两者之间抖。
const _: () =
    assert!((FLOOR_NEAR + FLOOR_FAR) * 0.5 < -1.0);

/// 轮廓上的一圈点:(倒角圆心那一圈的位置, 朝外的法线),从正面看逆时针。
///
/// 倒角是一条半圆,圆心那一圈比外缘内缩一个倒角半径;外缘的圆角半径因此等于
/// 这一圈的圆角半径加上倒角半径。
fn outline() -> Vec<([f32; 2], [f32; 2])> {
    let bevel = THICKNESS * 0.5;
    let core = HALF - bevel;
    let radius = (CORNER - bevel).max(0.002);
    let inner = core - radius;
    let centers = [
        (inner, inner),
        (-inner, inner),
        (-inner, -inner),
        (inner, -inner),
    ];
    let mut ring =
        Vec::with_capacity(4 * (CORNER_SEGMENTS + 1));
    for (quarter, (cx, cy)) in
        centers.into_iter().enumerate()
    {
        for step in 0..=CORNER_SEGMENTS {
            let angle = FRAC_PI_2
                * (quarter as f32
                    + step as f32 / CORNER_SEGMENTS as f32);
            let (sin, cos) = angle.sin_cos();
            ring.push((
                [cx + cos * radius, cy + sin * radius],
                [cos, sin],
            ));
        }
    }
    ring
}

/// 封面的平面投影:正面左上角是 (0, 0),图像的行向下走。
fn planar_uv(x: f32, y: f32) -> [f32; 2] {
    [0.5 + x / (2.0 * HALF), 0.5 - y / (2.0 * HALF)]
}

/// 封套:正面一片、背面一片,中间一圈半圆倒角把两片接起来。
///
/// 三角形一律从外面看逆时针(bevy 的正面),背面剔除照常开着。
pub(crate) fn slab() -> Vertices {
    let ring = outline();
    let count = ring.len();
    let bevel = THICKNESS * 0.5;
    let mut v = Vertices {
        positions: Vec::new(),
        normals: Vec::new(),
        uvs: Vec::new(),
        indices: Vec::new(),
    };
    // 正面:圆心一点加一圈,扇形。
    let front_center =
        push(&mut v, [0.0, 0.0, bevel], [0.0, 0.0, 1.0]);
    let front = v.positions.len();
    for &([x, y], _) in &ring {
        push(&mut v, [x, y, bevel], [0.0, 0.0, 1.0]);
    }
    for i in 0..count {
        v.indices.extend([
            front_center,
            index_of(front + i),
            index_of(front + (i + 1) % count),
        ]);
    }

    // 倒角:从正面(φ = 90°)转到背面(φ = −90°),每一圈都是同一条轮廓往外鼓出
    // b·cos φ、往前后挪 b·sin φ。第一圈与正面那圈位置重合、法线也同为 +z,
    // 接缝处因此连续。
    let bevel_start = v.positions.len();
    for k in 0..=BEVEL_SEGMENTS {
        let phi = FRAC_PI_2
            - PI * k as f32 / BEVEL_SEGMENTS as f32;
        let (sin, cos) = phi.sin_cos();
        for &([x, y], [nx, ny]) in &ring {
            push(
                &mut v,
                [
                    x + nx * bevel * cos,
                    y + ny * bevel * cos,
                    bevel * sin,
                ],
                [nx * cos, ny * cos, sin],
            );
        }
    }
    for k in 0..BEVEL_SEGMENTS {
        for i in 0..count {
            let at = |k: usize, i: usize| {
                index_of(
                    bevel_start + k * count + i % count,
                )
            };
            let (a, b, c, d) = (
                at(k, i),
                at(k, i + 1),
                at(k + 1, i + 1),
                at(k + 1, i),
            );
            v.indices.extend([a, d, c, a, c, b]);
        }
    }

    // 背面:与正面同形,绕序反过来。
    let back_center =
        push(&mut v, [0.0, 0.0, -bevel], [0.0, 0.0, -1.0]);
    let back = v.positions.len();
    for &([x, y], _) in &ring {
        push(&mut v, [x, y, -bevel], [0.0, 0.0, -1.0]);
    }
    for i in 0..count {
        v.indices.extend([
            back_center,
            index_of(back + (i + 1) % count),
            index_of(back + i),
        ]);
    }
    v
}

/// 关于 y = 0 平面的镜像:位置与法线的 y 取反,uv 不动,每个三角形的绕序反过来。
///
/// 倒影用的就是这一份。镜像本身会把绕序翻过来,反转索引是为了把它翻回去 ——
/// 这样倒影实体的变换仍是一个正常的旋转(镜像的那一次已经烘进几何里),
/// 背面剔除不必为它特殊处理。uv 不动,于是倒影里的封面是倒着的:那正是镜子里该有的样子。
pub(crate) fn mirrored(source: Vertices) -> Vertices {
    let flip = |[x, y, z]: [f32; 3]| [x, -y, z];
    Vertices {
        positions: source
            .positions
            .into_iter()
            .map(flip)
            .collect(),
        normals: source
            .normals
            .into_iter()
            .map(flip)
            .collect(),
        uvs: source.uvs,
        indices: source
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[a, b, c]| [a, c, b])
            .collect(),
    }
}

/// 地面:台面单位里 y = 0 的一块水平矩形,正面朝上。uv 横向随 x、纵深随 z
/// (远沿 0、近沿 1),着色器靠它算脚印的位置(见 [`footprint`])。
pub(crate) fn floor() -> Vertices {
    let corners = [
        [-FLOOR_HALF_WIDTH, FLOOR_NEAR],
        [FLOOR_HALF_WIDTH, FLOOR_NEAR],
        [FLOOR_HALF_WIDTH, FLOOR_FAR],
        [-FLOOR_HALF_WIDTH, FLOOR_FAR],
    ];
    Vertices {
        positions: corners
            .iter()
            .map(|&[x, z]| [x, 0.0, z])
            .collect(),
        normals: vec![[0.0, 1.0, 0.0]; 4],
        uvs: corners
            .iter()
            .map(|&[x, z]| {
                [
                    (x + FLOOR_HALF_WIDTH)
                        / (2.0 * FLOOR_HALF_WIDTH),
                    (z - FLOOR_FAR)
                        / (FLOOR_NEAR - FLOOR_FAR),
                ]
            })
            .collect(),
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

/// 封套正下方那一点在地面 uv 里的位置,以及「一个台面单位合多少 uv」(横, 纵)。
///
/// 着色器拿 `(uv − 中心) / 每单位` 换回台面单位,接触阴影与那一汪暗面才是按
/// 封套的尺寸画的,不随地面铺多大而变。
pub(crate) fn footprint() -> [f32; 4] {
    let span_x = 2.0 * FLOOR_HALF_WIDTH;
    let span_z = FLOOR_NEAR - FLOOR_FAR;
    [0.5, -FLOOR_FAR / span_z, 1.0 / span_x, 1.0 / span_z]
}

/// 加一个顶点(uv 按位置平面投影),交回它的下标。
fn push(v: &mut Vertices, p: [f32; 3], n: [f32; 3]) -> u32 {
    v.positions.push(p);
    v.normals.push(n);
    v.uvs.push(planar_uv(p[0], p[1]));
    index_of(v.positions.len() - 1)
}

fn index_of(i: usize) -> u32 {
    u32::try_from(i)
        .expect("封套只有几百个顶点,远小于 u32 上限")
}

#[cfg(test)]
mod tests;
