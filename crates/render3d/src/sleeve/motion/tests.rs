use super::*;

const FRAME: f32 = 1.0 / 60.0;

// ── 呼吸 ──────────────────────────────────────────────────────────

/// 呼吸的摆幅不超过 ±3°(左右)与 ±2°(前后):再大就是在晃。
#[test]
fn idle_breathing_stays_within_a_few_degrees() {
    for step in 0..2000 {
        let (pitch, yaw) = idle(step as f32 * 0.1);
        assert!(yaw.abs() <= IDLE_YAW + 1e-6);
        assert!(pitch.abs() <= IDLE_PITCH + 1e-6);
    }
}

/// 呼吸真的在动 —— 一个恒为零的 `idle` 会让上面那条照样全绿。
#[test]
fn idle_breathing_moves() {
    let a = idle(0.0);
    let b = idle(IDLE_YAW_PERIOD / 4.0);
    assert!((a.1 - b.1).abs() > IDLE_YAW * 0.5);
}

// ── 拖动与回弹 ────────────────────────────────────────────────────

/// 往右拖是往右转(yaw 为正),往下拖是顶边朝人(pitch 为正),同点云的手感。
#[test]
fn dragging_turns_the_sleeve_the_way_the_finger_goes() {
    let mut tilt = Tilt::default();
    tilt.drag(40.0, 25.0, FRAME);
    let (pitch, yaw) = tilt.angles();
    assert!(yaw > 0.0, "往右拖该往右转,yaw = {yaw}");
    assert!(
        pitch > 0.0,
        "往下拖该顶边朝人,pitch = {pitch}"
    );
}

/// 拖得再远也转不过上限(橡皮筋):背面没印东西,不该被拖成正对人。
#[test]
fn dragging_is_bounded_by_the_rubber_band() {
    let mut tilt = Tilt::default();
    for _ in 0..500 {
        tilt.drag(300.0, -300.0, FRAME);
    }
    let (pitch, yaw) = tilt.angles();
    assert!(yaw > 0.0 && yaw < MAX_YAW, "yaw = {yaw}");
    assert!(
        pitch < 0.0 && pitch > -MAX_PITCH,
        "pitch = {pitch}"
    );
}

/// 按着不动时不回弹:角度由手指说了算。
#[test]
fn a_held_sleeve_does_not_spring_back() {
    let mut tilt = Tilt::default();
    tilt.drag(80.0, 0.0, FRAME);
    let before = tilt.angles();
    for _ in 0..60 {
        tilt.step(FRAME);
    }
    assert_eq!(tilt.angles(), before);
}

/// 松手后回到原位,而且是冲过头一点再停(阻尼比小于 1)—— 不是被吸回去。
#[test]
fn a_released_sleeve_springs_back_with_a_small_overshoot() {
    let mut tilt = Tilt::default();
    tilt.drag(120.0, 0.0, FRAME);
    tilt.release();
    let mut crossed = false;
    for _ in 0..240 {
        tilt.step(FRAME);
        crossed |= tilt.angles().1 < 0.0;
    }
    assert!(crossed, "回弹该冲过原位一点");
    assert_eq!(
        tilt.angles(),
        (0.0, 0.0),
        "四秒后该停在原位"
    );
}

/// 回弹到一半又按住,角度从当前位置接着走,不跳回拖动前的原始量。
#[test]
fn grabbing_mid_spring_does_not_jump() {
    let mut tilt = Tilt::default();
    tilt.drag(150.0, 0.0, FRAME);
    tilt.release();
    for _ in 0..6 {
        tilt.step(FRAME);
    }
    let (_, before) = tilt.angles();
    tilt.drag(0.0, 0.0, FRAME);
    let (_, after) = tilt.angles();
    assert!(
        (after - before).abs() < 1e-4,
        "{before} → {after}"
    );
}

/// 慢帧(半秒一帧)切成小步积分,弹簧不会炸。
#[test]
fn a_slow_frame_does_not_blow_up_the_spring() {
    let mut tilt = Tilt::default();
    tilt.drag(200.0, 200.0, FRAME);
    tilt.release();
    tilt.step(0.5);
    let (pitch, yaw) = tilt.angles();
    assert!(
        pitch.abs() <= MAX_PITCH && yaw.abs() <= MAX_YAW
    );
}

/// 坏输入不污染状态。
#[test]
fn non_finite_input_is_ignored() {
    let mut tilt = Tilt::default();
    tilt.drag(f32::NAN, f32::INFINITY, FRAME);
    tilt.release();
    tilt.step(f32::NAN);
    tilt.step(-1.0);
    assert_eq!(tilt.angles(), (0.0, 0.0));
}

// ── 交接 ──────────────────────────────────────────────────────────

/// 首曲没有可退场的旧封套。
#[test]
fn a_fresh_exchange_is_already_done() {
    let exchange = Exchange::default();
    assert!(!exchange.running());
    assert_eq!(exchange.progress(), 1.0);
}

/// 交接在 EXCHANGE_SECS 内走完(600–900ms 那一档),计满即停。
#[test]
fn an_exchange_runs_for_its_duration_and_stops() {
    assert!((0.6..=0.9).contains(&EXCHANGE_SECS));
    let mut exchange = Exchange::default();
    exchange.start();
    assert!(exchange.running());
    assert_eq!(exchange.progress(), 0.0);
    exchange.advance(EXCHANGE_SECS * 0.5);
    assert!((exchange.progress() - 0.5).abs() < 1e-5);
    exchange.advance(10.0);
    assert!(!exchange.running());
    assert_eq!(exchange.progress(), 1.0);
}

/// 两端对得上:新封套在终点落回原位,旧封套在起点就在原位 ——
/// 否则交接开始或结束那一帧会跳。
#[test]
fn exchange_offsets_meet_the_rest_pose_at_the_ends() {
    assert_eq!(incoming(1.0), Offset::default());
    assert_eq!(outgoing(0.0), Offset::default());
}

/// 新封套从旧封套后面来,全程不比原位更近太多(只冲过头一点):
/// 两张不互相穿过。
#[test]
fn the_incoming_sleeve_stays_behind_until_it_settles() {
    assert!(incoming(0.0).translation.z < -0.5);
    for step in 0..=100 {
        let z = incoming(step as f32 / 100.0).translation.z;
        assert!(z < 0.1, "进度 {step}% 时冲到 z = {z}");
    }
}

/// 旧封套是被甩出去的:越到后面走得越快,终点离开了原位一大截。
#[test]
fn the_outgoing_sleeve_accelerates_away() {
    let a = outgoing(0.25).translation.length();
    let b = outgoing(0.5).translation.length();
    let c = outgoing(0.75).translation.length();
    assert!(c - b > b - a, "该是加速的");
    assert!(outgoing(1.0).translation.x < -1.0);
}

// ── 光与显影 ──────────────────────────────────────────────────────

/// 边缘光起得快、落得慢。
#[test]
fn glow_rises_faster_than_it_falls() {
    let mut glow = Glow::default();
    glow.follow(1.0, 0.1);
    let risen = glow.level();
    let mut fall = Glow::default();
    fall.follow(1.0, 100.0);
    fall.follow(0.0, 0.1);
    let fallen = 1.0 - fall.level();
    assert!(risen > fallen, "起 {risen},落 {fallen}");
}

/// 空封套上的显影匀速走完,不越过目标。
#[test]
fn a_reveal_fades_in_and_stops_at_the_target() {
    let mut reveal = Reveal::default();
    reveal.fade_to(1.0);
    assert!(reveal.printed());
    reveal.advance(REVEAL_SECS * 0.5);
    assert!((reveal.value() - 0.5).abs() < 1e-5);
    reveal.advance(10.0);
    assert_eq!(reveal.value(), 1.0);
    reveal.snap(0.0);
    assert_eq!(reveal.value(), 0.0);
    assert!(!reveal.printed());
}

// ── 台面 ──────────────────────────────────────────────────────────

/// 宽屏按高度放:封套边长占可视高度的 FIT_HEIGHT。
#[test]
fn a_wide_window_fits_the_sleeve_by_height() {
    let scale = stage_scale(1920, 1080, false);
    assert!(
        (scale - FIT_HEIGHT * visible_height()).abs()
            < 1e-4
    );
}

/// 竖屏按宽度收:小米13 竖屏左右都不出画,还留着边。
#[test]
fn a_phone_fits_the_sleeve_by_width() {
    let (w, h) = (1080u32, 2400u32);
    let scale = stage_scale(w, h, true);
    let wide = visible_height() * w as f32 / h as f32;
    assert!(
        scale < FIT_HEIGHT * visible_height(),
        "竖屏没收"
    );
    assert!(
        scale <= 0.75 * wide,
        "封套占了 {} 的宽",
        scale / wide
    );
}

/// 0 尺寸不除零。
#[test]
fn a_zero_sized_window_falls_back_to_the_height_fit() {
    for (w, h) in [(0, 1080), (1920, 0), (0, 0)] {
        let scale = stage_scale(w, h, false);
        assert!(scale.is_finite() && scale > 0.0);
    }
}
