//! 播放链路上的 PCM 整形:统一采样格式,以及把播放中的采样分一份出来的 tee。
//!
//! tee 的那一支如今只给频谱分析([`crate::spectrum`])。当初另一支喂同播的 Opus
//! 编码,同播已删(#137)。

use std::sync::mpsc;
use std::time::Duration;

use rodio::source::UniformSourceIterator;
use rodio::{ChannelCount, Sample, SampleRate, Source};

/// 播放链路统一的采样率。
///
/// 歌曲多半是 44.1kHz,这一步转换因此总会发生。48kHz 是同播时代为 Opus 定的,
/// 留着它是因为 [`crate::buffered`] 的缓冲按它算容量,换数不改变任何行为。
pub const OUTPUT_SAMPLE_RATE: u32 = 48_000;

/// 播放链路统一的声道数。
pub const OUTPUT_CHANNELS: u16 = 2;

/// 把任意来源统一成播放链路的采样率与声道数。
///
/// **必须在 [`crate::buffered`] 之前套上**:它交出的源对外声称 48kHz 立体声,
/// 格式对不上的话听到的是变调、变速或只剩半边声道的声音,而这一路上每一环
/// 单看都"没报错"。
pub fn normalize<S: Source>(
    source: S,
) -> UniformSourceIterator<S> {
    UniformSourceIterator::new(
        source,
        ChannelCount::new(OUTPUT_CHANNELS)
            .expect("声道数是编译期常量,非零"),
        SampleRate::new(OUTPUT_SAMPLE_RATE)
            .expect("采样率是编译期常量,非零"),
    )
}

/// 把一路音频原样传下去,同时复制一份到支路。
///
/// 频谱要跟着正在放的声音走:扬声器和分析器拿到的必须是同一批采样。
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
    /// 支路是**有界**的。无界的话,一个不再读取的消费者会让内存无限涨;
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
        // try_send 而非 send:支路满了(分析器读不动)或断了时,
        // **丢掉这个采样**而不是等。扬声器不该为支路上的故障停下来。
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

#[cfg(test)]
mod tests;
