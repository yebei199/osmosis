use similar_asserts::assert_eq;

use super::super::*;
use crate::stream_source::fixtures::*;

/// 跳转要穿过通道抵达另一头的解码器。
///
/// 这是整条链上唯一认识"第几秒"的地方 —— rodio 手里只有一条采样通道,
/// 通道不认识时间。
#[test]
fn a_seek_reaches_the_source_behind_the_channel() {
    let mut source = buffered_with(marker(0.0, true), 8);

    source
        .try_seek(Duration::from_secs(3))
        .expect("能跳的源该收下这次跳转");

    assert!(
        pull_until(&mut source, 3.0).is_some(),
        "跳完该听到第 3 秒的采样"
    );
}

/// 缓冲里躺着的旧采样必须丢掉。
///
/// 不丢的话,跳过去之后要先把那最多 5 秒的旧声音放完 ——
/// 听起来是"拖了进度条,声音过几秒才跟上",而进度条早就跳走了。
#[test]
fn samples_queued_before_a_seek_are_thrown_away() {
    /// 攒得足够多,好让"丢了"与"挨个嚼完"两种结果分得开。
    const CAPACITY: usize = 1000;
    /// 跳转之前那些采样的值。刻意不取 0.0 —— 那是欠数据时补的静音,
    /// 两者同值的话,"嚼旧货"与"等新货"在断言里长得一模一样。
    const STALE: Sample = 0.5;
    /// 跳到第几秒。[`Marker`] 跳完之后放的采样就是这个数。
    const TARGET: u64 = 7;

    let mut source =
        buffered_with(marker(STALE, true), CAPACITY);
    // 先让解码线程把缓冲灌满旧采样,不然没有对照
    std::thread::sleep(Duration::from_millis(50));

    source
        .try_seek(Duration::from_secs(TARGET))
        .expect("能跳的源该收下这次跳转");

    let deadline =
        std::time::Instant::now() + SEEK_DEADLINE;
    let mut stale = 0;
    let mut arrived = false;
    while std::time::Instant::now() < deadline {
        let Some(sample) = source.next() else { break };
        if sample == TARGET as Sample {
            arrived = true;
            break;
        }
        if sample == STALE {
            stale += 1;
        }
    }

    assert!(arrived, "跳完该听到第 {TARGET} 秒的采样");
    assert_eq!(
        stale, 0,
        "跳转之前攒下的采样一个都不该再放出来"
    );
}

/// **跳不动是当场就知道的事,当场说。**
///
/// `ForwardOnly` 之类的失败不读网络,微秒级就返回。裁决窗口内等得到它,
/// 于是 `try_seek` 如实返回 `Err` —— 这一条是「进度条说谎」的解药:
/// rodio 的 `TrackPosition` 只在 `Ok` 时才把位置挪过去,返回 `Err`
/// 它就根本不动,界面上的数字与声音因此始终对得上。
#[test]
fn a_fast_failure_comes_back_as_an_error() {
    let mut source = buffered_with(marker(0.25, false), 8);

    let err = source
        .try_seek(Duration::from_secs(3))
        .expect_err("跳不动该在裁决窗口内如实报回来");

    assert!(
        matches!(err, SeekError::NotSupported { .. }),
        "该原样转出解码器那句话,实际 {err}"
    );
    assert!(
        !source.seek_state().is_seeking(),
        "裁决已经交给调用方了,不该再挂着「在跳」"
    );
}

/// **真在取字节的那种慢,不能把声卡回调拖下水。**
///
/// 裁决等不到就乐观放行:那时位置确实会先跳过去,但那是**对的** ——
/// 它真的在往那儿去。结论随后由 [`SeekState`] 补上。窗口必须有界,
/// `try_seek` 是在声卡回调里被调的。
#[test]
fn a_slow_seek_is_let_through_without_blocking() {
    /// 取字节要多久。必须远大于 [`SEEK_VERDICT_WINDOW`],
    /// 不然量不出"没等满"这件事。
    const FETCH: Duration = Duration::from_millis(400);

    let mut source = buffered_with(
        Marker {
            at: 0.0,
            seekable: true,
            delay: FETCH,
        },
        8,
    );

    let started = std::time::Instant::now();
    source
        .try_seek(Duration::from_secs(3))
        .expect("等不到裁决就该乐观放行");
    let waited = started.elapsed();

    assert!(
        waited < FETCH,
        "这一等花在声卡回调里,不能等满整个取字节的时间(实际 {waited:?})"
    );
    assert!(
        source.seek_state().is_seeking(),
        "放行之后仍在跳,结论得留给 SeekState"
    );
    assert!(
        pull_until(&mut source, 3.0).is_some(),
        "字节取回来之后该听到新位置"
    );
}

/// 调用方没等到的那条裁决,要留在 [`SeekState`] 上。
///
/// 慢 + 失败是最难说清的一种:`try_seek` 那时已经乐观返回了,
/// 没有这条路的话,失败就彻底沉默 —— 界面会一直挂着「缓冲中」。
#[test]
fn a_failure_the_caller_missed_is_left_on_the_state() {
    let mut source = buffered_with(
        Marker {
            at: 0.25,
            seekable: false,
            delay: Duration::from_millis(400),
        },
        8,
    );
    let state = source.seek_state();

    source
        .try_seek(Duration::from_secs(3))
        .expect("裁决还没出来,这一下该先放行");

    let why = wait_for_failure(&state)
        .expect("没人接住的失败该留在状态上");
    assert!(
        why.contains("not supported"),
        "该说清是跳不了,实际 {why}"
    );
}

/// 跳不动的源不能把歌掐掉。
///
/// 为一次跳不动就结束这一首,比跳不动本身更糟。
#[test]
fn a_source_that_cannot_seek_says_so_and_keeps_playing() {
    /// 一个不会与静音(0.0)混淆的采样值,好认出源还在往下放。
    const STILL_PLAYING: Sample = 0.25;

    let mut source =
        buffered_with(marker(STILL_PLAYING, false), 8);

    // 跳不动这件事本身由上面那条测,这里只管它有没有把歌掐掉
    let _ = source.try_seek(Duration::from_secs(3));

    assert!(
        pull_until(&mut source, STILL_PLAYING).is_some(),
        "跳不动不该把这一首掐掉"
    );
}
