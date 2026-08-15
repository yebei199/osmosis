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
    assert!((l.bass - 1.0).abs() < 1e-3, "低段 {}", l.bass);
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
    assert_eq!(v.corners.len(), particles * CUBE_VERTICES);
    assert_eq!(v.tangents.len(), particles * CUBE_VERTICES);
    assert_eq!(v.indices.len(), particles * CUBE_INDICES);
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
    let top = (CLOUD_GRID * CLOUD_GRID - 1) * CUBE_VERTICES;
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
        assert_eq!(seen.len(), 8, "粒子 {cube} 的八角不全");
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
