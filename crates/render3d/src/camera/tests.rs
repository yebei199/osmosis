use super::*;
// 取投影矩阵的 trait,不在 prelude 里。
use bevy::camera::CameraProjection;

/// 一个世界点在这台相机里落到视口的哪儿:归一到 0..1,**左上**原点。
///
/// 复刻 bevy 的投影链(裁剪矩阵 × 相机逆变换,再做透视除),不需要 GPU 也不需要 `App`。
fn viewport_of(
    lens: &Lens,
    width: u32,
    height: u32,
    point: Vec3,
) -> Vec2 {
    let projection = PerspectiveProjection {
        fov: lens.fov,
        aspect_ratio: width as f32 / height as f32,
        ..Default::default()
    };
    let clip_from_view = match &lens.sub_view {
        Some(sub) => {
            projection.get_clip_from_view_for_sub(sub)
        }
        None => projection.get_clip_from_view(),
    };
    let view_from_world =
        Transform::from_translation(BASE_CAMERA_POS)
            .looking_at(Vec3::ZERO, Vec3::Y)
            .to_matrix()
            .inverse();
    let ndc = (clip_from_view * view_from_world)
        .project_point3(point);
    Vec2::new(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5)
}

const WIDE: (u32, u32) = (1600, 900);
const PHONE: (u32, u32) = (1080, 2400);

/// 宽版式:主轴落在窗宽 0.3 处(右边让给歌名与歌词),高度居中。
#[test]
fn the_wide_lens_puts_the_subject_left_of_centre() {
    let (w, h) = WIDE;
    let lens = lens(w, h, false);
    let at = viewport_of(&lens, w, h, Vec3::ZERO);
    assert!(
        (at.x - (0.5 - WIDE_SHIFT)).abs() < 1e-3,
        "主轴该在 x = {},实际 {at}",
        0.5 - WIDE_SHIFT
    );
    assert!(
        (at.y - 0.5).abs() < 1e-3,
        "高度该居中,实际 {at}"
    );
}

/// 紧凑版式:主轴往上让(字摆在下面),左右居中。
#[test]
fn the_compact_lens_puts_the_subject_above_centre() {
    let (w, h) = PHONE;
    let lens = lens(w, h, true);
    let at = viewport_of(&lens, w, h, Vec3::ZERO);
    assert!(
        (at.x - 0.5).abs() < 1e-3,
        "左右该居中,实际 {at}"
    );
    assert!(
        (at.y - (0.5 - COMPACT_SHIFT)).abs() < 1e-3,
        "主轴该在 y = {},实际 {at}",
        0.5 - COMPACT_SHIFT
    );
}

/// 移轴只平移、不缩放:同一个点离主轴多远,与不移轴时一模一样。
///
/// 这条守的是纵向移轴那一处视野换算 —— bevy 把视野对齐到整幅画面的高,
/// 漏掉换算的话紧凑版式里的东西会整体放大 `1 + 2·shift` 倍,而上面两条
/// 只量主轴落点,照样全绿。
#[test]
fn shifting_the_lens_does_not_rescale_the_picture() {
    let flat = Lens {
        sub_view: None,
        fov: BASE_FOV,
    };
    for (compact, (w, h)) in [(false, WIDE), (true, PHONE)]
    {
        let shifted = lens(w, h, compact);
        for point in [
            Vec3::new(1.0, 0.7, 0.0),
            Vec3::new(-0.6, -1.3, 1.8),
        ] {
            let base = viewport_of(&flat, w, h, point)
                - viewport_of(&flat, w, h, Vec3::ZERO);
            let moved = viewport_of(&shifted, w, h, point)
                - viewport_of(&shifted, w, h, Vec3::ZERO);
            assert!(
                (base - moved).length() < 1e-3,
                "compact = {compact}:{point} 离主轴 {moved},不移轴时是 {base}"
            );
        }
    }
}

/// 0 尺寸(首帧、刚重建)不移轴,也不让子视图带着 0 宽进投影里除零。
#[test]
fn a_zero_sized_target_gets_the_plain_lens() {
    for (w, h) in [(0, 900), (1600, 0), (0, 0)] {
        for compact in [false, true] {
            let lens = lens(w, h, compact);
            assert_eq!(lens.sub_view, None);
            assert_eq!(lens.fov, BASE_FOV);
        }
    }
}
