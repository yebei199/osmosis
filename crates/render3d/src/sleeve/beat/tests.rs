use super::*;

const FRAME: f32 = 1.0 / 60.0;

/// 喂 `frames` 帧同一个电平,返回这期间触发了几次起音。
fn feed(
    onset: &mut Onset,
    level: f32,
    frames: usize,
) -> usize {
    (0..frames)
        .filter(|_| onset.detect(level, FRAME) > 0.0)
        .count()
}

// ── 起音 ──────────────────────────────────────────────────────────

/// 安静里冒出来的一下鼓是一次起音,强度随高出多少而定。
#[test]
fn a_bass_hit_out_of_quiet_is_an_onset() {
    let mut onset = Onset::new(BASS_ONSET);
    assert_eq!(feed(&mut onset, 0.05, 60), 0);
    let strength = onset.detect(0.6, FRAME);
    assert!(
        strength > 0.9,
        "从 0.05 跳到 0.6 该是满强度,实际 {strength}"
    );
}

/// 一段持续的低音只在开头算一次:平均跟上来以后,它就是背景。
#[test]
fn sustained_bass_does_not_retrigger() {
    let mut onset = Onset::new(BASS_ONSET);
    feed(&mut onset, 0.05, 60);
    assert_eq!(feed(&mut onset, 0.7, 180), 1);
}

/// 一下鼓的能量在几帧里连涨,只算一次(不应期 + 边沿触发)。
#[test]
fn a_hit_rising_over_several_frames_counts_once() {
    let mut onset = Onset::new(BASS_ONSET);
    feed(&mut onset, 0.05, 60);
    let hits = [0.3, 0.45, 0.6, 0.7, 0.75]
        .into_iter()
        .filter(|level| onset.detect(*level, FRAME) > 0.0)
        .count();
    assert_eq!(hits, 1);
}

/// 一拍一拍的鼓(每拍 0.5s,鼓点之间落回安静)每拍都算。
#[test]
fn every_beat_of_a_steady_pattern_is_an_onset() {
    let mut onset = Onset::new(BASS_ONSET);
    feed(&mut onset, 0.05, 60);
    let mut hits = 0;
    for _ in 0..8 {
        hits += feed(&mut onset, 0.6, 6);
        feed(&mut onset, 0.05, 24);
    }
    assert_eq!(hits, 8);
}

/// 很安静的段落里那点起伏不算鼓点(电平下限)。
#[test]
fn a_whisper_is_not_a_beat() {
    let mut onset = Onset::new(BASS_ONSET);
    feed(&mut onset, 0.01, 60);
    assert_eq!(feed(&mut onset, 0.08, 10), 0);
}

/// 重置之后的第一帧只立平均:藏了一阵再切回来,不会拿 0 去比出一次假起音。
#[test]
fn a_reset_detector_does_not_fire_on_its_first_sample() {
    let mut onset = Onset::new(BASS_ONSET);
    onset.reset();
    assert_eq!(onset.detect(0.8, FRAME), 0.0);
    assert_eq!(feed(&mut onset, 0.8, 30), 0);
}

/// 时钟定格(dt = 0)与坏电平不推进状态,也不触发。
#[test]
fn a_frozen_clock_or_bad_level_is_ignored() {
    let mut onset = Onset::new(BASS_ONSET);
    feed(&mut onset, 0.05, 60);
    assert_eq!(onset.detect(0.9, 0.0), 0.0);
    assert_eq!(onset.detect(f32::NAN, FRAME), 0.0);
    assert_eq!(onset.detect(0.9, f32::NAN), 0.0);
    assert!(
        onset.detect(0.9, FRAME) > 0.0,
        "坏帧不该吃掉下一下鼓"
    );
}

// ── 鼓点那一下 ────────────────────────────────────────────────────

/// 满强度那一下:峰值约为 1,来得快(50ms 内到顶),去得也快(250ms 后看不出)。
#[test]
fn a_kick_peaks_quickly_and_settles_within_a_quarter_second()
 {
    let mut kick = Kick::default();
    kick.strike(1.0);
    let mut peak: f32 = 0.0;
    let mut peak_at = 0.0;
    let mut t = 0.0;
    while t < 0.25 {
        kick.step(1.0 / 240.0);
        t += 1.0 / 240.0;
        if kick.push() > peak {
            peak = kick.push();
            peak_at = t;
        }
    }
    assert!((0.95..=1.05).contains(&peak), "峰值 {peak}");
    assert!(peak_at < 0.05, "{peak_at}s 才到顶");
    assert!(
        kick.push().abs() < 0.05 * peak,
        "250ms 后还剩 {}",
        kick.push()
    );
}

/// 临界阻尼:回去的时候不往后缩 —— 冲过头就是一下变两下,鼓密了就是抖。
#[test]
fn a_kick_does_not_recoil_past_rest() {
    let mut kick = Kick::default();
    kick.strike(1.0);
    for _ in 0..120 {
        kick.step(FRAME);
        assert!(
            kick.push() >= 0.0,
            "缩到了 {}",
            kick.push()
        );
    }
}

/// 强度成比例:半强度的那一下峰值约为满强度的一半。
#[test]
fn a_weak_onset_gives_a_smaller_kick() {
    let peak = |strength: f32| {
        let mut kick = Kick::default();
        kick.strike(strength);
        (0..60)
            .map(|_| {
                kick.step(1.0 / 240.0);
                kick.push()
            })
            .fold(0.0_f32, f32::max)
    };
    let ratio = peak(0.5) / peak(1.0);
    assert!((ratio - 0.5).abs() < 0.05, "比值 {ratio}");
}

/// 鼓点再密也叠不过上限。
#[test]
fn dense_kicks_stay_capped() {
    let mut kick = Kick::default();
    for _ in 0..40 {
        kick.strike(1.0);
        kick.step(1.0 / 30.0);
        assert!(kick.push() <= KICK_CAP + 1e-6);
    }
}

/// 慢帧(半秒一帧)照样精确,不炸。
#[test]
fn a_slow_frame_does_not_blow_up_the_kick() {
    let mut kick = Kick::default();
    kick.strike(1.0);
    kick.step(0.5);
    assert!(kick.push().abs() < 0.05, "{}", kick.push());
}

// ── 摆幅 ──────────────────────────────────────────────────────────

/// 安静时摆幅就是原来的呼吸(1 倍)。
#[test]
fn a_quiet_passage_keeps_the_idle_sway() {
    let mut sway = Sway::default();
    for _ in 0..600 {
        sway.follow(0.05, FRAME);
    }
    assert_eq!(sway.amplitude(), 1.0);
}

/// 响的段落唱上几秒,摆幅明显放大,但不越过封顶。
#[test]
fn a_loud_passage_widens_the_sway_up_to_the_cap() {
    let mut sway = Sway::default();
    for _ in 0..600 {
        sway.follow(0.9, FRAME);
    }
    let amplitude = sway.amplitude();
    assert!(amplitude > 2.0, "{amplitude}");
    assert!(amplitude <= 1.0 + SWAY_BOOST + 1e-6);
}

/// 摆幅跟的是段落不是字:一下 0.1s 的响不会把摆幅拉起来多少。
#[test]
fn a_single_loud_moment_barely_moves_the_sway() {
    let mut sway = Sway::default();
    for _ in 0..6 {
        sway.follow(1.0, FRAME);
    }
    assert!(
        sway.amplitude() < 1.15,
        "{}",
        sway.amplitude()
    );
}

// ── 闪光 ──────────────────────────────────────────────────────────

/// 闪一下:立刻亮到起音的强度,0.3s 后看不见。
#[test]
fn a_spark_flashes_and_fades() {
    let mut spark = Spark::default();
    spark.strike(0.8);
    assert_eq!(spark.level(), 0.8);
    for _ in 0..18 {
        spark.step(FRAME);
    }
    assert!(spark.level() < 0.05, "{}", spark.level());
}

/// 正亮着时来一下更弱的,不会把它压暗。
#[test]
fn a_weaker_spark_does_not_dim_a_brighter_one() {
    let mut spark = Spark::default();
    spark.strike(0.9);
    spark.strike(0.3);
    assert_eq!(spark.level(), 0.9);
}
