//! 同播用的 Opus 编解码,以及把播放中的 PCM 分一份出来的 tee。
//!
//! 放在 `audio` 而不是 `syncplay`:采样、声道、帧长都是声音的事,
//! 传输层不该知道什么是采样 —— 它只负责把一串字节送到对面。
//!
//! **Opus 只接受固定长度的帧**(2.5/5/10/20/40/60 ms),而 rodio 的 `Source`
//! 一次给一个采样。中间那段攒帧的逻辑因此是必须的,也是最容易写错的地方。

use std::sync::mpsc;
use std::time::Duration;

use rodio::{ChannelCount, Sample, SampleRate, Source};

use crate::AudioError;

/// 同播链路统一的采样率。
///
/// Opus 内部就在 48kHz 上工作,喂别的采样率它会自己重采样 —— 与其让它猜,
/// 不如在 rodio 那一侧就转好。歌曲多半是 44.1kHz,所以这一步转换总会发生。
pub const SYNC_SAMPLE_RATE: u32 = 48_000;

/// 同播链路统一的声道数。
pub const SYNC_CHANNELS: u16 = 2;

/// 一帧的时长。
///
/// 20ms 是 WebRTC 的惯例:再短则包头占比升高,再长则每次丢包损失更多、延迟更大。
pub const FRAME_DURATION: Duration =
    Duration::from_millis(20);

/// 一帧里每个声道有多少个采样。
pub const FRAME_SAMPLES_PER_CHANNEL: usize =
    (SYNC_SAMPLE_RATE as usize) * 20 / 1000;

/// 一帧总共有多少个采样(交错存放)。
pub const FRAME_SAMPLES: usize =
    FRAME_SAMPLES_PER_CHANNEL * SYNC_CHANNELS as usize;

/// 把一路音频原样传下去,同时复制一份到支路。
///
/// 主控要边放边推:本机的扬声器和听众拿到的必须是同一批采样。
pub struct Tee<S> {
    inner: S,
    branch: mpsc::SyncSender<Sample>,
}

impl<S> Tee<S>
where
    S: Source,
{
    /// 包住一路音频,返回它与支路的接收端。
    ///
    /// 支路是**有界**的。无界的话,一个不再读取的听众会让内存无限涨;
    /// 而有界 + 丢弃(见 [`Iterator::next`] 的实现)保证本机播放永不被拖慢。
    pub fn new(
        inner: S,
        capacity: usize,
    ) -> (Self, mpsc::Receiver<Sample>) {
        let (branch, receiver) =
            mpsc::sync_channel(capacity);
        (Self { inner, branch }, receiver)
    }
}

impl<S> Iterator for Tee<S>
where
    S: Source,
{
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        let sample = self.inner.next()?;
        // try_send 而非 send:支路满了(听众读不动)或断了(听众走了)时,
        // **丢掉这个采样**而不是等。本机的扬声器不该为远端的故障停下来。
        let _ = self.branch.try_send(sample);
        Some(sample)
    }
}

impl<S> Source for Tee<S>
where
    S: Source,
{
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}

/// 把 PCM 攒成整帧再编码成 Opus。
pub struct Encoder {
    encoder: opus::Encoder,
    /// 攒够 [`FRAME_SAMPLES`] 才能编一帧。
    pending: Vec<f32>,
}

impl Encoder {
    pub fn new() -> Result<Self, AudioError> {
        // Audio 而非 Voip:推的是音乐,不是通话。Voip 那档会为了语音清晰度
        // 砍掉音乐里的高频与立体声细节。
        let encoder = opus::Encoder::new(
            SYNC_SAMPLE_RATE,
            opus::Channels::Stereo,
            opus::Application::Audio,
        )
        .map_err(|e| AudioError::Decode(e.to_string()))?;

        Ok(Self {
            encoder,
            pending: Vec::with_capacity(FRAME_SAMPLES * 2),
        })
    }

    /// 喂一批采样,吐出这批里凑得出的完整帧。
    ///
    /// 返回 `Vec` 而非单帧:一次喂进来的量与帧长没有对齐关系,
    /// 可能一帧都凑不满,也可能凑出好几帧。
    pub fn push(
        &mut self,
        samples: &[Sample],
    ) -> Result<Vec<Vec<u8>>, AudioError> {
        self.pending.extend_from_slice(samples);

        let mut frames = Vec::new();
        // Opus 帧的上界:60ms 立体声也就几千字节,给足余量一次分配。
        let mut buffer = vec![0u8; 4_000];
        while self.pending.len() >= FRAME_SAMPLES {
            let written = self
                .encoder
                .encode_float(
                    &self.pending[..FRAME_SAMPLES],
                    &mut buffer,
                )
                .map_err(|e| {
                    AudioError::Decode(e.to_string())
                })?;
            frames.push(buffer[..written].to_vec());
            // 用 drain 而非 split_off:剩下的不足一帧要留在原地等下一批,
            // 丢掉它们会让每次调用的边界处都缺一小段,听起来是周期性的爆音。
            self.pending.drain(..FRAME_SAMPLES);
        }

        Ok(frames)
    }
}

/// 把 Opus 帧解回 PCM。
pub struct Decoder {
    decoder: opus::Decoder,
}

impl Decoder {
    pub fn new() -> Result<Self, AudioError> {
        let decoder = opus::Decoder::new(
            SYNC_SAMPLE_RATE,
            opus::Channels::Stereo,
        )
        .map_err(|e| AudioError::Decode(e.to_string()))?;

        Ok(Self { decoder })
    }

    /// 解一帧。
    pub fn decode(
        &mut self,
        frame: &[u8],
    ) -> Result<Vec<Sample>, AudioError> {
        let mut out = vec![0f32; FRAME_SAMPLES];
        // fec=false:丢包补偿要配合前一帧的信息,现在没有重传也没有 jitter buffer,
        // 开了只会拿到一段猜出来的音频。真做补偿是另一件事。
        let decoded = self
            .decoder
            .decode_float(frame, &mut out, false)
            .map_err(|e| {
                AudioError::Decode(e.to_string())
            })?;

        // decoded 是**每声道**的采样数,截断时要乘回声道数。
        out.truncate(decoded * SYNC_CHANNELS as usize);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    /// 一段可辨认的测试信号:440Hz 正弦,双声道。
    fn tone(frames: usize) -> Vec<f32> {
        (0..frames * SYNC_CHANNELS as usize)
            .map(|i| {
                let t = (i / SYNC_CHANNELS as usize) as f32
                    / SYNC_SAMPLE_RATE as f32;
                (t * 440.0 * core::f32::consts::TAU).sin()
                    * 0.5
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
        let copied: Vec<Sample> =
            branch.try_iter().collect();

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
        let mut encoder =
            Encoder::new().expect("建不了编码器");

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
        let mut encoder =
            Encoder::new().expect("建不了编码器");
        let mut decoder =
            Decoder::new().expect("建不了解码器");

        let original = tone(FRAME_SAMPLES_PER_CHANNEL * 4);
        let frames =
            encoder.push(&original).expect("编码失败");
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
        let mut decoder =
            Decoder::new().expect("建不了解码器");

        assert!(matches!(
            decoder.decode(&[0xff, 0xff, 0xff, 0xff]),
            Err(AudioError::Decode(_))
        ));
    }
}
