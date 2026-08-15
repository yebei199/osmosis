use similar_asserts::assert_eq;

use super::super::*;
use crate::stream_source::fixtures::*;

/// **落点跳不动就往前挪一秒再试一次。**
///
/// mp3 的比特池接不上时,落在那一帧的解码必然失败,而往前一两帧就没事 ——
/// 一秒是它的四十倍余量。不重试的话用户看到的是"这首歌回不去",
/// 而它其实只差几十毫秒。
#[test]
fn a_stuck_seek_is_retried_one_second_earlier() {
    /// 用户拖到的时刻。
    const TARGET: Duration = Duration::from_secs(30);
    /// 卡住的边界:目标那一刻跳不动,往前一秒(第 29 秒)跳得动。
    const STUCK_AFTER: Duration =
        Duration::from_millis(29_500);

    let (tape, attempts) = tape(Some(STUCK_AFTER));
    let mut source = buffered_with(tape, 8);

    source.try_seek(TARGET).expect("回退重试之后该跳成");

    assert!(
        pull_until(&mut source, 30.0).is_some(),
        "重试之后该听到第 30 秒的采样"
    );
    assert_eq!(
        attempt_count(&attempts),
        2,
        "该是原地一次、回退一秒一次"
    );
}

/// **重试成功之后,位置要落在用户要的那一刻,不是往前挪的那一刻。**
///
/// rodio 的 `TrackPosition` 拿到 `Ok` 就把位置设成**目标值**。落点若真在
/// 目标前一秒,进度条会一直偏一秒直到下次跳转,歌词跟着一起偏。
/// 所以回退之后要向前解码丢弃到目标点 —— 那 1 秒也正好把比特池填满。
#[test]
fn the_retry_discards_forward_to_the_requested_position() {
    const TARGET: Duration = Duration::from_secs(30);
    const STUCK_AFTER: Duration =
        Duration::from_millis(29_500);

    let (tape, _) = tape(Some(STUCK_AFTER));
    let mut source = buffered_with(tape, 8);

    source.try_seek(TARGET).expect("回退重试之后该跳成");

    let first = first_real_sample(&mut source)
        .expect("跳完该有声音");
    assert_eq!(
        first, 30.0,
        "落点在第 29 秒,但交出去的第一个采样必须已经走到第 30 秒"
    );
}

/// **只重试一次,失败就如实说。**
///
/// 回退一秒还跳不动,就不是比特池的事了(格式不支持、这条流只进不退),
/// 再往前退也一样失败,白白多花一次取字节。
#[test]
fn a_seek_that_fails_twice_is_reported_and_not_retried_again()
 {
    // 哪儿都跳不动 —— 回退一秒改变不了任何事
    let (tape, attempts) = tape(Some(Duration::ZERO));
    let mut source = buffered_with(tape, 8);

    let err = source
        .try_seek(Duration::from_secs(30))
        .expect_err("两次都跳不动就该如实报回来");

    assert!(
        matches!(err, SeekError::NotSupported { .. }),
        "该原样转出解码器那句话,实际 {err}"
    );
    assert_eq!(
        attempt_count(&attempts),
        2,
        "回退一次就够,不该没完没了地往前退"
    );
}

/// **边界:目标不足一秒时,回退落在 0,不下溢。**
///
/// `Duration` 的减法会 panic,而歌的开头恰恰是最常被拖到的地方之一。
#[test]
fn a_retry_near_the_start_clamps_to_zero() {
    /// 拖到开头附近。回退一秒会越过 0。
    const TARGET: Duration = Duration::from_millis(500);
    const STUCK_AFTER: Duration =
        Duration::from_millis(400);

    let (tape, attempts) = tape(Some(STUCK_AFTER));
    let mut source = buffered_with(tape, 8);

    source.try_seek(TARGET).expect("回退到 0 之后该跳成");

    let first = first_real_sample(&mut source)
        .expect("跳完该有声音");
    assert_eq!(
        first, 0.5,
        "落点被夹到 0,再向前丢弃到第 0.5 秒"
    );
    assert_eq!(attempt_count(&attempts), 2);
}
