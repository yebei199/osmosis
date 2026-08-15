use similar_asserts::assert_eq;

use super::fixtures::*;
use super::*;

/// 听同播时没有解码器可跳,`try_seek` 必须报错而不是假装跳了。
///
/// 假装跳了的话进度条会跳走,而声音还在原地 —— 两个说法对不上。
#[test]
fn a_listener_channel_cannot_be_seeked() {
    let (_tx, rx) = mpsc::channel::<Sample>();
    let mut source = ChannelSource::new(rx);

    let err = source
        .try_seek(Duration::from_secs(3))
        .expect_err("没有解码器就跳不了");

    assert!(
        matches!(err, SeekError::NotSupported { .. }),
        "该如实说不支持,实际 {err}"
    );
}

/// 跳转期间对外是"正在跳",跳完回到"没事"。界面靠它显示「缓冲中」。
#[test]
fn the_seek_state_reports_while_it_is_fetching() {
    /// 假源取字节要多久。够长到断言跑得完,够短到测试不难受。
    const FETCH: Duration = Duration::from_millis(200);

    let mut source = buffered_with(
        Marker {
            at: 0.0,
            seekable: true,
            delay: FETCH,
        },
        8,
    );
    let state = source.seek_state();
    assert!(!state.is_seeking(), "还没跳,不该说在跳");

    source
        .try_seek(Duration::from_secs(3))
        .expect("能跳的源该收下这次跳转");

    assert!(
        state.is_seeking(),
        "请求送出去之后、字节到位之前,对外就该是「在跳」"
    );
    assert!(pull_until(&mut source, 3.0).is_some());
    assert!(!state.is_seeking(), "跳完了就不该再说在跳");
}

/// 通道里的采样原样播出。
#[test]
fn channel_source_yields_what_was_sent() {
    let (tx, rx) = mpsc::channel();
    for sample in [0.1, -0.2, 0.3] {
        tx.send(sample).expect("发不进通道");
    }
    drop(tx);

    let played: Vec<Sample> =
        ChannelSource::new(rx).collect();

    assert_eq!(played, vec![0.1, -0.2, 0.3]);
}

/// **通道暂时空时给静音,不能结束。**
///
/// 返回 `None` 对 rodio 就是"这首放完了",它会把源丢掉 ——
/// 于是一次几十毫秒的网络抖动会让听众**永久**没声,而连接一切正常。
#[test]
fn channel_source_fills_underrun_with_silence() {
    let (tx, rx) = mpsc::channel();
    tx.send(0.5).expect("发不进通道");
    // 发送端还活着,只是暂时没有新数据 —— 正是抖动时的样子。

    let mut source = ChannelSource::new(rx);

    assert_eq!(source.next(), Some(0.5));
    assert_eq!(
        source.next(),
        Some(0.0),
        "欠数据时该给静音而不是结束"
    );
    assert_eq!(source.next(), Some(0.0));
}

/// 发送端关闭才是真的结束。
#[test]
fn channel_source_ends_when_channel_closes() {
    let (tx, rx) = mpsc::channel::<Sample>();
    drop(tx);

    let mut source = ChannelSource::new(rx);

    // 关闭时若还欠着静音,先把它补完;补完必须结束。
    let played: Vec<Sample> =
        source.by_ref().take(SILENCE_BURST * 2).collect();
    assert!(
        played.len() < SILENCE_BURST * 2,
        "发送端关闭后不该无限产出静音"
    );
    assert_eq!(source.next(), None);
}
