use similar_asserts::assert_eq;

use super::*;

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
        assert!(now >= last, "转回去了: {last} -> {now}");
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
    assert_eq!(spin.angles(), before, "坏 dt 推进了惯性");
}
