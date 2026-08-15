//! 同播用的 Opus 编解码,以及把播放中的 PCM 分一份出来的 tee。
//!
//! 放在 `audio` 而不是 `syncplay`:采样、声道、帧长都是声音的事,
//! 传输层不该知道什么是采样 —— 它只负责把一串字节送到对面。
//!
//! **Opus 只接受固定长度的帧**(2.5/5/10/20/40/60 ms),而 rodio 的 `Source`
//! 一次给一个采样。中间那段攒帧的逻辑因此是必须的,也是最容易写错的地方。

use std::sync::mpsc;
use std::time::Duration;

use rodio::source::UniformSourceIterator;
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

/// 支路的容量,以采样计。约 200ms(48kHz 立体声),与听众侧的缓冲同一个量级。
pub const BRANCH_CAPACITY: usize =
    SYNC_SAMPLE_RATE as usize * SYNC_CHANNELS as usize / 5;

/// 把任意来源统一成同播链路的采样率与声道数。
///
/// **必须在 [`Tee`] 之前套上。** 歌基本都是 44.1kHz,也有单声道的,而 [`Encoder`]
/// 硬性假设 48kHz 立体声 —— 不转的话支路流出的采样与它错位,听众听到的是变调、
/// 变速或只剩半边声道的声音,而这一路上每一环单看都"没报错"。
///
/// 顺带让本机播放也走同一份采样:主控听到的和推出去的因此逐采样相同,
/// 出问题时不必再问"是不是转换那一步的锅"。
pub fn normalize<S: Source>(
    source: S,
) -> UniformSourceIterator<S> {
    UniformSourceIterator::new(
        source,
        ChannelCount::new(SYNC_CHANNELS)
            .expect("声道数是编译期常量,非零"),
        SampleRate::new(SYNC_SAMPLE_RATE)
            .expect("采样率是编译期常量,非零"),
    )
}

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

    /// 跳转照常传下去。
    ///
    /// 不写这一条的话拿到的是 trait 的默认实现 —— 一句「不支持」。而 [`Tee`]
    /// 只是分了一支采样出去,凭什么让整条链失去跳转能力(真实症状:进度条
    /// 一拖就报 `Seeking is not supported by source: Tee<Tee<...>>`)。
    fn try_seek(
        &mut self,
        pos: Duration,
    ) -> Result<(), rodio::source::SeekError> {
        self.inner.try_seek(pos)
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
mod tests;
