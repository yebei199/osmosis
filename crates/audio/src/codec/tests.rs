use similar_asserts::assert_eq;

use super::*;

/// 一段可辨认的测试信号:440Hz 正弦,双声道。
fn tone(frames: usize) -> Vec<f32> {
    (0..frames * SYNC_CHANNELS as usize)
        .map(|i| {
            let t = (i / SYNC_CHANNELS as usize) as f32
                / SYNC_SAMPLE_RATE as f32;
            (t * 440.0 * core::f32::consts::TAU).sin() * 0.5
        })
        .collect()
}

/// 一路可以当 `Source` 用的采样。
fn source(
    samples: Vec<f32>,
) -> rodio::buffer::SamplesBuffer {
    rodio::buffer::SamplesBuffer::new(
        ChannelCount::new(SYNC_CHANNELS)
            .expect("声道数是编译期常量,非零"),
        SampleRate::new(SYNC_SAMPLE_RATE)
            .expect("采样率是编译期常量,非零"),
        samples,
    )
}

/// **[`Tee`] 必须把跳转传下去。**
///
/// 不传的话拿到的是 trait 默认的那句「不支持」,而 `Tee` 只是分了一支
/// 采样出去 —— 整条链凭什么因此失去跳转。真实症状是进度条一拖就报
/// `Seeking is not supported by source: Tee<Tee<...>>`。
#[test]
fn tee_passes_a_seek_down_to_its_inner_source() {
    // SamplesBuffer 是能跳的,所以「跳得动」这件事只取决于 Tee 转不转发
    let (mut tee, _branch) =
        Tee::new(source(tone(4_800)), BRANCH_CAPACITY);

    assert!(
        tee.try_seek(Duration::from_millis(10)).is_ok(),
        "Tee 该把跳转交给里面那一路"
    );
}

/// **归一必须真的换算,不能只是改个标称值。**
///
/// 44.1kHz 单声道是最常见的那一类不匹配。只报格式不换算的话,
/// [`Encoder`] 会把 44100 个采样当成 48000 个来切帧 —— 听众听到的是
/// 快了 9% 的变调音,而链路上没有任何一环报错。
#[test]
fn normalize_forces_the_sync_format() {
    // 100ms 的单声道 44.1kHz。
    let odd = rodio::buffer::SamplesBuffer::new(
        ChannelCount::new(1).expect("非零"),
        SampleRate::new(44_100).expect("非零"),
        vec![0.5f32; 4_410],
    );

    let normalized = normalize(odd);
    assert_eq!(
        normalized.sample_rate().get(),
        SYNC_SAMPLE_RATE
    );
    assert_eq!(normalized.channels().get(), SYNC_CHANNELS);

    // 同样是 100ms,换算后该是 48000×0.1×2 个采样。重采样的边界处理
    // 各实现差几个采样,所以给 1% 的余量而不是钉死。
    let produced = normalized.count();
    let expected = SYNC_SAMPLE_RATE as usize
        * SYNC_CHANNELS as usize
        / 10;
    assert!(
        produced.abs_diff(expected) < expected / 100,
        "100ms 换算后应有约 {expected} 个采样,实得 {produced}"
    );
}

/// tee 不能吃掉任何采样 —— 主路少一个采样,本机放出来的声音就缺一块。
#[test]
fn tee_forwards_every_sample_downstream() {
    let samples = tone(10);
    let (tee, _branch) =
        Tee::new(source(samples.clone()), 4096);

    let forwarded: Vec<Sample> = tee.collect();

    assert_eq!(forwarded.len(), samples.len());
}

/// 支路拿到的是同一批采样。
#[test]
fn tee_copies_samples_to_the_branch() {
    let samples = tone(10);
    let (tee, branch) =
        Tee::new(source(samples.clone()), 4096);

    let forwarded: Vec<Sample> = tee.collect();
    let copied: Vec<Sample> = branch.try_iter().collect();

    assert_eq!(copied, forwarded);
}

/// **支路满了不能拖慢主路。**
///
/// 听众断线后没人再读支路,它很快就满。若此时 tee 阻塞等待,
/// 本机的音乐会跟着卡住 —— 一个远端的故障拖垮了本地播放,
/// 而现象("音乐一顿一顿")离病因("某个听众没了")极远。
#[test]
fn tee_survives_a_full_branch() {
    let samples = tone(100);
    // 容量远小于样本数,且**从不读取**:支路必定溢出。
    let (tee, branch) =
        Tee::new(source(samples.clone()), 8);
    drop(branch);

    let forwarded: Vec<Sample> = tee.collect();

    assert_eq!(
        forwarded.len(),
        samples.len(),
        "支路满/断开时主路必须照常走完"
    );
}

/// 攒够一帧才编一帧,凑不满的留着。
#[test]
fn encoder_emits_fixed_duration_frames() {
    let mut encoder = Encoder::new().expect("建不了编码器");

    // 半帧:一帧都编不出。
    let half = encoder
        .push(&tone(FRAME_SAMPLES_PER_CHANNEL / 2))
        .expect("编码失败");
    assert!(half.is_empty(), "不足一帧不该产出");

    // 再来两帧半的量:连同上次剩的,总共该出三帧。
    let rest = encoder
        .push(&tone(FRAME_SAMPLES_PER_CHANNEL * 5 / 2))
        .expect("编码失败");
    assert_eq!(rest.len(), 3);
    assert!(
        rest.iter().all(|frame| !frame.is_empty()),
        "编出来的帧不该是空的"
    );
}

/// 编码再解码,信号还在。
///
/// Opus 有损,不能比字节。判据是**长度对得上**且**能量没塌** ——
/// 静音、全零、单声道错位这几种典型故障都会让能量掉到接近零。
#[test]
fn round_trip_preserves_the_signal() {
    let mut encoder = Encoder::new().expect("建不了编码器");
    let mut decoder = Decoder::new().expect("建不了解码器");

    let original = tone(FRAME_SAMPLES_PER_CHANNEL * 4);
    let frames = encoder.push(&original).expect("编码失败");
    assert!(!frames.is_empty(), "四帧的量该编出帧");

    let mut decoded = Vec::new();
    for frame in &frames {
        decoded.extend(
            decoder.decode(frame).expect("解码失败"),
        );
    }

    assert_eq!(
        decoded.len(),
        frames.len() * FRAME_SAMPLES,
        "解出来的采样数应与帧数对得上"
    );

    let energy: f32 =
        decoded.iter().map(|s| s * s).sum::<f32>()
            / decoded.len() as f32;
    assert!(
        energy > 0.01,
        "解出来的信号能量塌了({energy}),多半是静音或声道错位"
    );
}

/// 坏帧报错,不 panic。丢包与乱序在真实网络上是常态。
#[test]
fn decoder_rejects_a_corrupt_frame() {
    let mut decoder = Decoder::new().expect("建不了解码器");

    assert!(matches!(
        decoder.decode(&[0xff, 0xff, 0xff, 0xff]),
        Err(AudioError::Decode(_))
    ));
}
