use similar_asserts::assert_eq;

use std::io::Cursor;

use rodio::Source as _;
use std::time::Duration;

use crate::codec;
use crate::stream_source::buffered_with;

use super::*;

mod reconnect;
mod stall;

/// 解一段内存里的音频,**长度如实传下去**。
///
/// 生产环境的长度来自 `Content-Length`(见 [`load_with`]),内存里的来自
/// `Vec::len` —— 两者对解码器是同一件事。测试走这条路,是为了让"能往回跳"
/// 这件事在测试里与真机里由同一个开关决定。
fn decode_cursor(
    bytes: Vec<u8>,
) -> Result<rodio::Decoder<Cursor<Vec<u8>>>, AudioError> {
    let len = bytes.len() as u64;
    decode(Cursor::new(bytes), Some(len))
}

/// 合成一段单声道 44.1kHz 的 WAV,`samples` 个采样点。
///
/// 手搓而不是引 hound:44 字节的头就能让测试独立于任何编码库,
/// 也让"截断"这种边界能被精确构造。
fn wav(samples: u32) -> Vec<u8> {
    const SAMPLE_RATE: u32 = 44_100;
    const CHANNELS: u16 = 1;
    const BITS: u16 = 16;

    let data_len = samples * u32::from(BITS / 8);
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt 块长度
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&CHANNELS.to_le_bytes());
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    let byte_rate = SAMPLE_RATE
        * u32::from(CHANNELS)
        * u32::from(BITS / 8);
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(
        &(CHANNELS * BITS / 8).to_le_bytes(),
    );
    out.extend_from_slice(&BITS.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..samples {
        // 一段随便什么波形,只要不是恒零,免得被解码器当成空块跳过。
        let sample = (i % 1000) as i16;
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// 内存里的 `Cursor` 与真实流句柄走同一条解码路径,
/// 因此这里读对了采样率和声道数,真实播放的格式协商也就是对的。
#[test]
fn decodes_wav_from_seekable_source() {
    let decoder = decode_cursor(wav(4_410))
        .expect("合法 WAV 应能解码");

    assert_eq!(decoder.sample_rate().get(), 44_100);
    assert_eq!(decoder.channels().get(), 1);
}

/// **知道长度的流才跳得回去。**
///
/// rodio 的默认设置是 `is_seekable: false` / `byte_len: None`,于是 symphonia
/// 把流当成只进不退,回跳返回 `ForwardOnly`。真机症状是往前拖得动、
/// 往回拖报「这首跳不了」,同一首歌两种结果 —— 而在此之前这条路
/// 一条测试都没有,这正是它溜过去的原因。
#[test]
fn a_decoder_with_a_known_length_seeks_backwards() {
    // 10 秒,好让回跳真的跨过若干帧而不是原地打转
    let mut decoder = decode_cursor(wav(441_000))
        .expect("合法 WAV 应能解码");

    // 先往前放一段。回跳正是默认设置下会失败的那一半 ——
    // 往前跳在只进不退的流上本来就成立,证明不了什么。
    decoder.by_ref().take(100_000).count();

    decoder
        .try_seek(Duration::from_secs(1))
        .expect("知道长度的流该跳得回去");
}

/// 长度未知时照常解码、照常放,只是跳不回去。
///
/// 守的是「拿不到 Content-Length 就整首放不了」这种过度反应 ——
/// 长度是**跳转**的前提,不是播放的前提。
#[test]
fn an_unknown_length_still_decodes_and_plays() {
    let decoder = decode(Cursor::new(wav(4_410)), None)
        .expect("长度未知也该解得开");

    assert!(
        decoder.count() > 0,
        "长度是跳转的前提,不是播放的前提"
    );
}

/// 直链过期时上游返回的是一个 HTML 错误页。必须报解码错误,
/// 让上层能提示「重新获取播放地址」,而不是 panic 掉整个 UI 线程。
#[test]
fn rejects_non_audio_source() {
    let html = b"<html><body>403 Forbidden</body></html>";

    // 用 matches! 而非 expect_err:rodio 的 Decoder 没有 Debug,
    // expect_err 要求 Ok 侧可 Debug,编不过。
    assert!(matches!(
        decode_cursor(html.to_vec()),
        Err(AudioError::Decode(_))
    ));
}

/// 零字节边界:服务端返回了 200 但正文是空的。
#[test]
fn rejects_empty_source() {
    assert!(matches!(
        decode_cursor(Vec::new()),
        Err(AudioError::Decode(_))
    ));
}

/// 流式独有的故障:头是完整的、数据只下来一半。
///
/// 关键不是"能否解码"(头齐全就能),而是**取样必须终止**。
/// 断流时若解码器一直等下去,UI 会停在「播放中」再也不动 ——
/// 那是最难排查的一类症状,所以在这里钉死。
#[test]
fn truncated_source_terminates_instead_of_hanging() {
    let full = wav(44_100);
    let truncated = full[..1_000].to_vec();

    let decoder = decode_cursor(truncated)
        .expect("头完整时应能建出解码器");

    // 头里声称有 44100 个采样点,实到不足 500 个。
    // 上界取声称值的两倍:真挂住的话这里会先耗尽而不是永远转下去。
    let produced = decoder.take(88_200).count();

    assert!(
        produced < 44_100,
        "截断的流不该产出完整时长的采样: 实得 {produced}"
    );
}
