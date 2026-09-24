use similar_asserts::assert_eq;

use super::*;

/// 一段可辨认的测试信号:440Hz 正弦,双声道。
fn tone(frames: usize) -> Vec<f32> {
    (0..frames * OUTPUT_CHANNELS as usize)
        .map(|i| {
            let t = (i / OUTPUT_CHANNELS as usize) as f32
                / OUTPUT_SAMPLE_RATE as f32;
            (t * 440.0 * core::f32::consts::TAU).sin() * 0.5
        })
        .collect()
}

/// 一路可以当 `Source` 用的采样。
fn source(
    samples: Vec<f32>,
) -> rodio::buffer::SamplesBuffer {
    rodio::buffer::SamplesBuffer::new(
        ChannelCount::new(OUTPUT_CHANNELS)
            .expect("声道数是编译期常量,非零"),
        SampleRate::new(OUTPUT_SAMPLE_RATE)
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
        Tee::new(source(tone(4_800)), 4096);

    assert!(
        tee.try_seek(Duration::from_millis(10)).is_ok(),
        "Tee 该把跳转交给里面那一路"
    );
}

/// **归一必须真的换算,不能只是改个标称值。**
///
/// 44.1kHz 单声道是最常见的那一类不匹配。只报格式不换算的话,
/// 下游把 44100 个采样当成 48000 个来放 —— 听到的是快了 9% 的变调音,
/// 而链路上没有任何一环报错。
#[test]
fn normalize_forces_the_output_format() {
    // 100ms 的单声道 44.1kHz。
    let odd = rodio::buffer::SamplesBuffer::new(
        ChannelCount::new(1).expect("非零"),
        SampleRate::new(44_100).expect("非零"),
        vec![0.5f32; 4_410],
    );

    let normalized = normalize(odd);
    assert_eq!(
        normalized.sample_rate().get(),
        OUTPUT_SAMPLE_RATE
    );
    assert_eq!(
        normalized.channels().get(),
        OUTPUT_CHANNELS
    );

    // 同样是 100ms,换算后该是 48000×0.1×2 个采样。重采样的边界处理
    // 各实现差几个采样,所以给 1% 的余量而不是钉死。
    let produced = normalized.count();
    let expected = OUTPUT_SAMPLE_RATE as usize
        * OUTPUT_CHANNELS as usize
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
/// 分析器读不动时支路很快就满。若此时 tee 阻塞等待,音乐会跟着卡住 ——
/// 可视化的故障拖垮了播放,而现象("音乐一顿一顿")离病因极远。
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
