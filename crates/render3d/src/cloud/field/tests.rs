use similar_asserts::assert_eq;

use super::*;
use crate::camera::lens;
// 取投影矩阵的 trait,不在 prelude 里。
use bevy::camera::CameraProjection;

const WIDE: (u32, u32) = (1920, 1080);
const PHONE: (u32, u32) = (1080, 2400);

/// 点云平面上一个点落到视口的哪儿:归一到 0..1,**左上**原点。复刻 bevy 的投影链,
/// 与 `camera/tests.rs` 里那份同构。
fn viewport_of(
    width: u32,
    height: u32,
    compact: bool,
    x: f32,
    y: f32,
) -> Vec2 {
    let lens = lens(width, height, compact);
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
        .project_point3(
            CLOUD_ORIGIN + Vec3::new(x, y, 0.0),
        );
    Vec2::new(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5)
}

// ── 视口框 ────────────────────────────────────────────────────────

/// 框的四角正好落在视口四角上:框算的与镜头实际拍到的是同一块。
/// 这条守的是「场铺满视口」的前提 —— 框算小了,右边那条空带就回来了。
#[test]
fn the_window_is_exactly_what_the_lens_sees() {
    for (compact, (w, h)) in [(false, WIDE), (true, PHONE)]
    {
        let window =
            Framing::of_viewport(w, h, compact).unwrap();
        let top_left = viewport_of(
            w,
            h,
            compact,
            window.left,
            window.top,
        );
        let bottom_right = viewport_of(
            w,
            h,
            compact,
            window.right,
            window.bottom,
        );
        assert!(
            top_left.length() < 1e-3,
            "compact = {compact}:左上角落在 {top_left}"
        );
        assert!(
            (bottom_right - Vec2::ONE).length() < 1e-3,
            "compact = {compact}:右下角落在 {bottom_right}"
        );
    }
}

/// 宽版式的病根:封面右边缘远在视口右边之内,右边要补的远多于左边。
#[test]
fn the_wide_layout_needs_far_more_on_the_right() {
    let window =
        Framing::of_viewport(WIDE.0, WIDE.1, false)
            .unwrap();
    assert!(window.right > PLANE_SIZE * 0.5 * 2.0);
    let field = Field::covering(&window);
    assert!(
        field.right > 4 * field.left,
        "左 {} 右 {}",
        field.left,
        field.right
    );
}

/// 竖屏紧凑版式:封面已经撑出左右与上边,只补下面。
#[test]
fn the_phone_layout_only_extends_downwards() {
    let field = Field::for_viewport(PHONE.0, PHONE.1, true);
    assert_eq!(
        (field.left, field.right, field.top),
        (0, 0, 0)
    );
    assert!(
        field.bottom > 10,
        "下边只补了 {}",
        field.bottom
    );
}

/// 场的外沿盖住视口的每一边(按格心算,再留一格给立方体的半边)。
#[test]
fn the_field_covers_the_window_on_every_side() {
    for (compact, (w, h)) in [
        (false, WIDE),
        (true, PHONE),
        (false, (1600, 1200)),
        (true, (900, 1000)),
    ] {
        let window =
            Framing::of_viewport(w, h, compact).unwrap();
        let field = Field::covering(&window);
        let half = PLANE_SIZE * 0.5;
        let reach =
            |cells: u32| half + cells as f32 * CELL_WORLD;
        assert!(
            reach(field.left) >= -window.left
                && reach(field.right) >= window.right
                && reach(field.bottom) >= -window.bottom
                && reach(field.top) >= window.top,
            "{w}x{h} compact = {compact}:{field:?} 盖不住 {window:?}"
        );
    }
}

/// 0 尺寸只有封面那一块,不除零。
#[test]
fn a_zero_sized_viewport_is_just_the_cover() {
    for (w, h) in [(0, 900), (1600, 0), (0, 0)] {
        assert_eq!(
            Field::for_viewport(w, h, false),
            Field::default()
        );
    }
    assert_eq!(Field::default().cubes(), 96 * 96);
}

/// 超宽的窗也有上限。
#[test]
fn an_absurdly_wide_window_is_capped() {
    let field = Field::for_viewport(20_000, 300, false);
    assert!(field.right <= MAX_CELLS);
}

/// 框正好是封面那一块时,它在 uv 里的四边正好是封面边上那一圈格心的 uv ——
/// 与 `mesh::cloud_vertices` 用的是同一条换算。
#[test]
fn the_cover_square_maps_to_its_edge_cell_uvs() {
    let half = PLANE_SIZE * 0.5;
    let square = Framing {
        left: -half,
        right: half,
        bottom: -half,
        top: half,
    };
    let uv = square.cover_uv();
    let edge = 0.5 / CLOUD_GRID as f32;
    for (got, want) in [
        (uv.x, edge),
        (uv.y, edge),
        (uv.z, 1.0 - edge),
        (uv.w, 1.0 - edge),
    ] {
        assert!((got - want).abs() < 1e-5, "{uv}");
    }
}

// ── 重建时机 ──────────────────────────────────────────────────────

fn field(right: u32) -> Field {
    Field {
        right,
        ..Field::default()
    }
}

/// 盖得住、多得不多:不重建。
#[test]
fn a_field_that_still_fits_is_kept() {
    let built = field(40);
    assert!(built.fits(&field(40)));
    assert!(built.fits(&field(40 - SHRINK_SLACK)));
    assert!(!built.fits(&field(41)), "盖不住了");
    assert!(
        !built.fits(&field(40 - SHRINK_SLACK - 1)),
        "多得太多了"
    );
}

/// 视口变了,等它稳住才重建,而且只建一次。
#[test]
fn a_change_is_rebuilt_once_after_it_settles() {
    let mut refit = Refit::new(field(40));
    assert_eq!(refit.step(field(40), 0.016), None);
    let frame = REFIT_SETTLE_SECS / 4.0;
    let mut built = Vec::new();
    for _ in 0..12 {
        built.extend(refit.step(field(60), frame));
    }
    assert_eq!(built, vec![field(60)]);
}

/// 拖着窗口边时视口每帧都在变:一直不建,停下来才建。
#[test]
fn a_window_being_dragged_is_not_rebuilt_every_frame() {
    let mut refit = Refit::new(field(40));
    for right in 41..80 {
        assert_eq!(refit.step(field(right), 0.016), None);
    }
    let mut built = Vec::new();
    for _ in 0..30 {
        built.extend(refit.step(field(79), 0.016));
    }
    assert_eq!(built, vec![field(79)]);
}

/// 变了又变回来(还配得上已建的那一圈):等着的那次作废。
#[test]
fn a_change_that_reverts_is_dropped() {
    let mut refit = Refit::new(field(40));
    refit.step(field(60), 0.1);
    assert_eq!(refit.step(field(40), 0.1), None);
    assert_eq!(refit.step(field(40), 1.0), None);
}

/// 时钟不走(dt 为 0 或坏值)时不会被当成「已经稳住」。
#[test]
fn a_frozen_clock_does_not_count_as_settled() {
    let mut refit = Refit::new(field(40));
    for dt in [0.0, f32::NAN, -1.0, 0.0, 0.0] {
        assert_eq!(refit.step(field(60), dt), None);
    }
}
