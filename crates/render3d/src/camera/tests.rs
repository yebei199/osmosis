use super::*;
use crate::marker;
use crate::scene::CLOUD_ORIGIN;
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
fn an_anchor_in_front_of_the_camera_projects_to_a_viewport_point()
 {
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
fn an_anchor_off_the_side_of_the_screen_has_no_viewport_point()
 {
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
    marker::ORBIT_PERIOD
        * f32::from(
            u16::try_from(step)
                .expect("采样点数远小于 u16 上限"),
        )
        / ORBIT_STEPS as f32
}

/// 标记体绕的那条轨道,近端离开封面平面、远端贴回去,且始终在相机这一侧。
/// 远端贴平面才有粒子成片从它前面过(遮挡演得出来),近端离开平面卡片才读得成。
#[test]
fn the_marker_orbits_between_the_cover_plane_and_the_camera()
 {
    let (mut nearest, mut farthest) = (f32::MIN, f32::MAX);
    for step in 0..ORBIT_STEPS {
        let z =
            marker::pose(orbit_time(step)).translation.z;
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
        let ndc = clip_from_world.project_point3(anchor);
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
    assert_eq!(occluder_depth(None), EMPTY_OCCLUDER_DEPTH);
    for z in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
        assert_eq!(
            occluder_depth(Some(Vec3::new(0.0, 0.0, z))),
            EMPTY_OCCLUDER_DEPTH,
            "NDC z = {z} 应该退回空遮挡层"
        );
    }
}
