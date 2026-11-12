use super::*;

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// 四个数组逐顶点等长,索引不越界、是整三角形。
fn assert_well_formed(v: &Vertices, what: &str) {
    let n = v.positions.len();
    assert_eq!(v.normals.len(), n, "{what}:法线数");
    assert_eq!(v.uvs.len(), n, "{what}:uv 数");
    assert_eq!(
        v.indices.len() % 3,
        0,
        "{what}:索引不是整三角形"
    );
    assert!(
        v.indices.iter().all(|&i| (i as usize) < n),
        "{what}:索引越界"
    );
}

/// 每个三角形从外面看都是逆时针:几何法线与顶点法线同向。
///
/// 背面剔除照常开着,绕序错一处就是一块面在屏幕上消失;倒影那份是镜像出来的,
/// 镜像会把绕序翻过来,这条同时守着「翻回去」那一步。
fn assert_faces_outward(v: &Vertices, what: &str) {
    for t in v.indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]]
            .map(|i| v.positions[i as usize]);
        let face = cross(sub(b, a), sub(c, a));
        if dot(face, face) < 1e-14 {
            // 圆角极小处的退化三角形没有方向可言。
            continue;
        }
        let normal = [t[0], t[1], t[2]]
            .map(|i| v.normals[i as usize])
            .into_iter()
            .fold([0.0; 3], |s, n| {
                [s[0] + n[0], s[1] + n[1], s[2] + n[2]]
            });
        assert!(
            dot(face, normal) > 0.0,
            "{what}:三角形 {t:?} 绕序朝里"
        );
    }
}

#[test]
fn the_slab_is_well_formed_and_faces_outward() {
    let slab = slab();
    assert_well_formed(&slab, "封套");
    assert_faces_outward(&slab, "封套");
}

#[test]
fn the_mirror_is_well_formed_and_faces_outward() {
    let mirror = mirrored(slab());
    assert_well_formed(&mirror, "倒影");
    assert_faces_outward(&mirror, "倒影");
}

#[test]
fn the_floor_faces_up() {
    let floor = floor();
    assert_well_formed(&floor, "地面");
    assert_faces_outward(&floor, "地面");
    assert!(
        floor.normals.iter().all(|n| *n == [0.0, 1.0, 0.0])
    );
}

/// 法线都是单位长:着色器的光照与「正面权重」按点积算,长度不对就整片偏亮偏暗。
#[test]
fn slab_normals_are_unit_length() {
    for n in slab().normals {
        assert!(
            (dot(n, n).sqrt() - 1.0).abs() < 1e-4,
            "法线 {n:?} 不是单位长"
        );
    }
}

/// 封套的外形:正面边长 1,厚度 THICKNESS,中心在原点。
#[test]
fn the_slab_spans_one_unit_by_its_thickness() {
    let slab = slab();
    let extent = |axis: usize| {
        let values = slab.positions.iter().map(|p| p[axis]);
        let max = values.clone().fold(f32::MIN, f32::max);
        let min = values.fold(f32::MAX, f32::min);
        (min, max)
    };
    for axis in [0, 1] {
        let (min, max) = extent(axis);
        assert!(
            (min + HALF).abs() < 1e-4
                && (max - HALF).abs() < 1e-4,
            "第 {axis} 轴范围 {min}..{max},该是 ±{HALF}"
        );
    }
    let (min, max) = extent(2);
    assert!(
        (max - min - THICKNESS).abs() < 1e-5,
        "厚度 {},该是 {THICKNESS}",
        max - min
    );
}

/// 封面不倒:正面左上角取图像左上角,正面右下角取右下角;uv 不出 [0, 1]。
#[test]
fn the_cover_is_upright_on_the_front() {
    let slab = slab();
    for (p, uv) in slab.positions.iter().zip(&slab.uvs) {
        assert!(
            uv.iter()
                .all(|c| (-1e-5..=1.0 + 1e-5).contains(c)),
            "uv {uv:?} 出界"
        );
        // 左上象限(x < 0, y > 0)只该采图像的左上象限。
        if p[0] < -0.1 && p[1] > 0.1 {
            assert!(
                uv[0] < 0.5 && uv[1] < 0.5,
                "{p:?} 采到 {uv:?}"
            );
        }
    }
}

/// 倒影是关于 y = 0 的镜像:同一个顶点 y 取反,uv 不动(镜子里的封面是倒着的)。
#[test]
fn the_mirror_flips_height_and_keeps_the_print() {
    let (slab, mirror) = (slab(), mirrored(slab()));
    for ((p, q), (a, b)) in slab
        .positions
        .iter()
        .zip(&mirror.positions)
        .zip(slab.uvs.iter().zip(&mirror.uvs))
    {
        assert_eq!([p[0], -p[1], p[2]], *q);
        assert_eq!(a, b);
    }
}

/// 脚印换回来正好是原点:着色器的接触阴影要落在封套正下方。
#[test]
fn the_footprint_maps_back_to_the_origin() {
    let floor = floor();
    let [cu, cv, per_u, per_v] = footprint();
    for (p, uv) in floor.positions.iter().zip(&floor.uvs) {
        let x = (uv[0] - cu) / per_u;
        let z = (uv[1] - cv) / per_v;
        assert!(
            (x - p[0]).abs() < 1e-4
                && (z - p[2]).abs() < 1e-4,
            "角 {p:?} 从 uv {uv:?} 换回 ({x}, {z})"
        );
    }
}
