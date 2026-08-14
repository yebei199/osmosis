use similar_asserts::assert_eq;

use super::*;

// ── 换歌过渡 ──────────────────────────────────────────────────────

/// 从未换过歌:颜色全给新封面、没有脉冲 —— 首曲没有「上一首」可渐变,
/// 起手就渐变会让第一首歌从一片占位色淡入。
#[test]
fn fresh_transition_shows_only_the_new_cover() {
    let t = TrackTransition::default();
    assert_eq!(t.color_mix(), 1.0);
    assert_eq!(t.burst(), 0.0);
}

/// 颜色渐变:换歌归零后单调走向 1,到头钳死不越界。
#[test]
fn color_mix_walks_from_old_to_new_and_stops_at_one() {
    let mut t = TrackTransition::default();
    t.start();
    assert_eq!(t.color_mix(), 0.0);

    let mut last = 0.0;
    for _ in 0..30 {
        t.advance(0.05);
        let now = t.color_mix();
        assert!(
            now >= last,
            "颜色混合倒退: {last} -> {now}"
        );
        assert!(
            (0.0..=1.0).contains(&now),
            "颜色混合越界: {now}"
        );
        last = now;
    }
    assert_eq!(last, 1.0, "推够时间后该完全是新封面");
}

/// burst:换歌瞬间满,单调衰减到 0 之后不反弹。
#[test]
fn burst_decays_to_zero_and_stays_there() {
    let mut t = TrackTransition::default();
    t.start();
    assert_eq!(t.burst(), 1.0);

    let mut last = 1.0;
    for _ in 0..30 {
        t.advance(0.05);
        let now = t.burst();
        assert!(now <= last, "脉冲反弹: {last} -> {now}");
        assert!(
            (0.0..=1.0).contains(&now),
            "脉冲越界: {now}"
        );
        last = now;
    }
    assert_eq!(last, 0.0, "推够时间后脉冲该归零");
}

/// 坏的 `dt`(NaN / 负数 / 无穷)不推进也不 panic,两条曲线仍然有界 ——
/// `dt` 来自两帧时间戳相减,时钟会被门冻结、也会被系统改。
#[test]
fn bad_delta_time_keeps_the_transition_bounded() {
    let mut t = TrackTransition::default();
    t.start();
    for dt in [f32::NAN, -1.0, f32::INFINITY, -0.0] {
        t.advance(dt);
        assert_eq!(
            t.color_mix(),
            0.0,
            "坏 dt {dt} 推进了过渡"
        );
        assert_eq!(t.burst(), 1.0);
    }
}
