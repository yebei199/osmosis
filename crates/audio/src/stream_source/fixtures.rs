//! 测试共用的假 Source 与驱动 helper。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{ChannelCount, Sample, SampleRate, Source};

use crate::codec::{SYNC_CHANNELS, SYNC_SAMPLE_RATE};

use super::*;

/// 一个位置读得出来的假源:每个采样的值就是"已经跳到了第几秒"。
///
/// 这样"跳成没成"不必去问被测代码自己 —— 直接从放出来的采样里读,
/// 而那正是真实链路上用户听到的东西。
pub(super) struct Marker {
    pub(super) at: Sample,
    /// 跳不跳得动。要演两种源:能跳的解码器,和跳不了的东西。
    pub(super) seekable: bool,
    /// 跳一次要多久。真实链路上跳到还没下到的位置要重开一个 range 请求,
    /// 而"缓冲中"这个状态只在那段时间里存在 —— 不让假源慢下来,
    /// 就没有那一段可看。
    pub(super) delay: Duration,
}

impl Iterator for Marker {
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        Some(self.at)
    }
}

impl Source for Marker {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        ChannelCount::new(SYNC_CHANNELS).expect("非零")
    }

    fn sample_rate(&self) -> SampleRate {
        SampleRate::new(SYNC_SAMPLE_RATE).expect("非零")
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }

    fn try_seek(
        &mut self,
        pos: Duration,
    ) -> Result<(), SeekError> {
        std::thread::sleep(self.delay);
        if !self.seekable {
            return Err(SeekError::NotSupported {
                underlying_source: "Marker",
            });
        }
        self.at = pos.as_secs_f32();
        Ok(())
    }
}

pub(super) fn marker(at: Sample, seekable: bool) -> Marker {
    Marker {
        at,
        seekable,
        delay: Duration::ZERO,
    }
}

/// 等解码线程给出跳转的结论。取走即清,所以只能问到一次。
pub(super) fn wait_for_failure(
    state: &SeekState,
) -> Option<String> {
    let deadline =
        std::time::Instant::now() + SEEK_DEADLINE;
    while std::time::Instant::now() < deadline {
        if let Some(why) = state.take_failure() {
            return Some(why);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

/// 跳转后多久必须听到新位置的声音。
///
/// 给得远大于一次线程唤醒,好让判据不受调度抖动左右;真的坏了也不会
/// 让这条测试挂在那里。
pub(super) const SEEK_DEADLINE: Duration =
    Duration::from_secs(2);

/// 一直取采样,直到读到 `want` 或者超时,返回取了多少个。
///
/// 轮询而不是睡一觉:解码线程什么时候跟上是调度说了算,
/// 固定的睡眠要么白等,要么在忙机器上假红。
pub(super) fn pull_until(
    source: &mut ChannelSource,
    want: Sample,
) -> Option<usize> {
    let deadline =
        std::time::Instant::now() + SEEK_DEADLINE;
    let mut pulled = 0;
    while std::time::Instant::now() < deadline {
        pulled += 1;
        if source.next() == Some(want) {
            return Some(pulled);
        }
    }
    None
}

/// 一秒有多少个交错采样。位置用**采样序号**存而不是拿 `Duration` 累加:
/// 浮点走上 96000 步之后就对不上整秒了,而断言要的正是整秒。
pub(super) const SAMPLES_PER_SECOND: u64 =
    SYNC_SAMPLE_RATE as u64 * SYNC_CHANNELS as u64;

/// 一条**会走**的假带子:每取一个采样,位置就前进一个采样的时长。
///
/// [`Marker`] 跳完之后永远吐同一个数,而"向前解码丢弃到目标点"这件事
/// 只有在位置会走的源上才看得见 —— 丢掉的那些采样,正是让位置从落点
/// 走到目标点的东西。放出来的采样值就是"现在是第几秒"。
///
/// `stuck_after` 演的是 mp3 的比特池:落在那一刻的那一帧要用前一帧留下的
/// 字节,解码器当场报错;往前挪一点再跳就没事了。
pub(super) struct Tape {
    /// 当前位置,交错采样序号。
    pub(super) at: u64,
    /// 跳到**不早于**这个时刻就失败。`None` = 哪儿都跳得动。
    pub(super) stuck_after: Option<Duration>,
    /// `try_seek` 被调了几次。用来证明"只重试一次"。
    pub(super) attempts: Arc<Mutex<usize>>,
}

impl Iterator for Tape {
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        let now =
            self.at as f64 / SAMPLES_PER_SECOND as f64;
        self.at += 1;
        Some(now as Sample)
    }
}

impl Source for Tape {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        ChannelCount::new(SYNC_CHANNELS).expect("非零")
    }

    fn sample_rate(&self) -> SampleRate {
        SampleRate::new(SYNC_SAMPLE_RATE).expect("非零")
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }

    fn try_seek(
        &mut self,
        pos: Duration,
    ) -> Result<(), SeekError> {
        *self
            .attempts
            .lock()
            .unwrap_or_else(|e| e.into_inner()) += 1;
        if self.stuck_after.is_some_and(|edge| pos >= edge)
        {
            return Err(SeekError::NotSupported {
                underlying_source: "Tape(比特池接不上)",
            });
        }
        self.at = (pos.as_secs_f64()
            * SAMPLES_PER_SECOND as f64)
            .round() as u64;
        Ok(())
    }
}

/// 建一条带子,并把它的尝试计数器一并交出来。
pub(super) fn tape(
    stuck_after: Option<Duration>,
) -> (Tape, Arc<Mutex<usize>>) {
    let attempts = Arc::new(Mutex::new(0));
    (
        Tape {
            at: 0,
            stuck_after,
            attempts: Arc::clone(&attempts),
        },
        attempts,
    )
}

/// 取到第一个不是静音的采样 —— 也就是跳完之后真正交出来的第一份声音。
///
/// 静音是通道暂时空时垫的,不代表位置。带子上第 0 个采样恰好也是 0.0,
/// 但那一个永远落在丢弃的那一段里,到不了这儿。
pub(super) fn first_real_sample(
    source: &mut ChannelSource,
) -> Option<Sample> {
    let deadline =
        std::time::Instant::now() + SEEK_DEADLINE;
    while std::time::Instant::now() < deadline {
        let sample = source.next()?;
        if sample != 0.0 {
            return Some(sample);
        }
    }
    None
}

/// 数一下 `try_seek` 被调了几次。
pub(super) fn attempt_count(
    counter: &Arc<Mutex<usize>>,
) -> usize {
    *counter.lock().unwrap_or_else(|e| e.into_inner())
}
