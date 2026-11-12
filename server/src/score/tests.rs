//! 喜爱度公式的单元测试:锚、单次播放的取值、衰减、有界的修正与合成。

use similar_asserts::assert_eq;

use super::*;

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "{actual} 不等于 {expected}"
    );
}

/// 今天放的、时长 200 秒、听了 `listened_ms`。
fn today(listened_ms: i64) -> Listen {
    Listen {
        listened_ms,
        duration_ms: 200_000,
        age_days: 0.0,
    }
}

#[test]
fn anchors_are_the_four_levels() {
    assert_eq!(anchor(None), 0);
    assert_eq!(anchor(Some(-1)), DISLIKED);
    assert_eq!(anchor(Some(1)), LIKED);
    assert_eq!(anchor(Some(2)), LOVED);
    assert_eq!(
        (DISLIKED, LIKED, LOVED),
        (-600, 400, 800),
        "锚是用户批准过的四档,改它要改 ADR 0042"
    );
}

#[test]
fn a_listen_reads_by_completion() {
    // 听完(九成以上)是 +1,一成以下是 -1,一半是 0,中间线性
    close(listen_value(200_000, 200_000).unwrap(), 1.0);
    close(listen_value(180_000, 200_000).unwrap(), 1.0);
    close(listen_value(100_000, 200_000).unwrap(), 0.0);
    close(listen_value(60_000, 200_000).unwrap(), -0.5);
    close(listen_value(20_000, 200_000).unwrap(), -1.0);
    close(listen_value(5_000, 200_000).unwrap(), -1.0);
    // 往回拖过、累计超过时长的按听完算
    close(listen_value(250_000, 200_000).unwrap(), 1.0);
}

#[test]
fn noise_and_unknown_lengths_say_nothing() {
    assert_eq!(listen_value(NOISE_MS - 1, 200_000), None);
    assert!(listen_value(NOISE_MS, 200_000).is_some());
    assert_eq!(listen_value(100_000, 0), None);
}

#[test]
fn weight_halves_every_ninety_days() {
    close(decay(0.0), 1.0);
    close(decay(90.0), 0.5);
    close(decay(180.0), 0.25);
    // 时钟偏差:未来的播放按今天算,不放大
    close(decay(-5.0), 1.0);
}

#[test]
fn correction_grows_with_listens_but_saturates() {
    assert_eq!(correction(Vec::<Listen>::new()), 0);
    assert_eq!(correction([today(200_000)]), 64);
    assert_eq!(correction([today(200_000); 3]), 152);
    // 一百遍也只逼近界,收在 ±199,不与锚的档距之半相接
    assert_eq!(correction([today(200_000); 100]), 199);
    assert_eq!(correction([today(5_000); 100]), -199);
    // 太短的那几下不进账
    assert_eq!(correction([today(1_000); 100]), 0);
}

#[test]
fn old_listens_weigh_less() {
    let old = Listen {
        age_days: HALF_LIFE_DAYS,
        ..today(200_000)
    };
    assert_eq!(correction([old]), 33);
}

/// 修正只在档内排先后:说过的话压得住听歌习惯。
#[test]
fn listening_never_crosses_a_spoken_level() {
    let loved_forever = [today(200_000); 1000];
    let skipped_forever = [today(5_000); 1000];
    assert_eq!(score(None, loved_forever), 199);
    assert_eq!(score(None, skipped_forever), -199);
    assert_eq!(score(Some(1), skipped_forever), 201);
    assert_eq!(score(Some(1), loved_forever), 599);
    assert_eq!(score(Some(2), skipped_forever), 601);
    assert_eq!(score(Some(2), loved_forever), 999);
    assert_eq!(score(Some(-1), loved_forever), -401);
    assert_eq!(score(Some(-1), skipped_forever), -799);
}

/// 与 `server/tests/facets.rs` 的 `every_facet_is_aggregated` 同一组播放:
/// 踩过,听了 30 秒、29.999 秒、90 秒(时长 200 秒)。
#[test]
fn a_disliked_track_with_three_short_listens() {
    let listens =
        [today(30_000), today(29_999), today(90_000)];
    assert_eq!(correction(listens), -111);
    assert_eq!(score(Some(-1), listens), -711);
}
