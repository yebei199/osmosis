//! `SyncSource`:包在媒体外面的 rodio 源，吐出去的每一帧都知道是媒体的哪一帧。
//!
//! 位置、跳转、暂停归它，不归 rodio 的 `Player`:
//! - rodio 的位置计数数的是经过它的采样，这一层内部丢帧、seek 它都不知道;
//! - rodio 的暂停与跳转都在它的周期访问里生效(5ms 一次),卡不准一帧。
//!
//! 所以 `Player` 这边始终是「放着」的，出不出声、从媒体哪里取都由这里决定。

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{ChannelCount, Sample, SampleRate, Source};

use super::follower::{Follower, Stats};
use super::timeline::Target;

/// 从媒体拉一个采样的结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pulled {
    Sample(Sample),
    /// 暂时没数据(网络慢了)。与真正的静音不同：它不占媒体时间，位置不能往前走。
    Starved,
    /// 媒体放完了。
    End,
}

/// 同步源底下真正的媒体。
pub trait Feed: Send + 'static {
    fn pull(&mut self) -> Pulled;
    /// 跳到媒体的 `to` 处。之后拉出来的第一个采样就是那一刻的。
    fn seek(&mut self, to: Duration) -> Result<(), SeekError>;
    fn channels(&self) -> ChannelCount;
    fn sample_rate(&self) -> SampleRate;
}

/// 把任意 rodio 源当成媒体：它从不欠载，拉不出来就是放完了。
pub struct SourceFeed<S>(pub S);

impl<S: Source + Send + 'static> Feed for SourceFeed<S> {
    fn pull(&mut self) -> Pulled {
        match self.0.next() {
            Some(sample) => Pulled::Sample(sample),
            None => Pulled::End,
        }
    }

    fn seek(&mut self, to: Duration) -> Result<(), SeekError> {
        self.0.try_seek(to)
    }

    fn channels(&self) -> ChannelCount {
        self.0.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.0.sample_rate()
    }
}

/// 一次跳转请求：跳到哪、裁决往哪回。
type SeekRequest = (Duration, mpsc::Sender<Result<(), SeekError>>);

/// 同步源与外面共用的那一份：时间线、输出后端报来的呈现时刻、当前位置。
///
/// 声卡回调线程与控制线程都碰它，但回调里只做原子读写与无竞争的短锁。
#[derive(Debug)]
pub struct SyncShared {
    target: Mutex<Target>,
    /// 输出后端每块报一次：块号、这一块第一帧的呈现时刻、这一块多少帧。
    block_seq: AtomicU64,
    block_present_ns: AtomicI64,
    block_frames: AtomicU64,
    /// 最近吐出去的那一帧是媒体的第几纳秒。
    position_ns: AtomicI64,
    /// 不跟时间线时的暂停：不出声、不消耗媒体。
    paused: AtomicBool,
    seek: Mutex<Option<SeekRequest>>,
    stats: Mutex<Report>,
}

/// 同步源此刻的状况，给上报与日志用。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Report {
    pub follower: Stats,
    /// 这一刻是不是在出声(不是静音、不是欠载、不是对齐中)。
    pub sounding: bool,
    /// 欠载过几次(一次欠载 = 连续拉不到数据的一段)。
    pub starves: u64,
}

impl SyncShared {
    pub fn new() -> Arc<Self> {
        todo!()
    }

    pub fn set_target(&self, target: Target) {
        let _ = target;
        todo!()
    }

    pub fn target(&self) -> Target {
        todo!()
    }

    /// 输出后端每块调一次：这一块第一帧的呈现时刻(本机单调时钟),以及这一块多少帧。
    pub fn block(&self, present_ns: i64, frames: u64) {
        let _ = (present_ns, frames);
        todo!()
    }

    /// 最近出声的那一帧在媒体的哪里。
    pub fn position(&self) -> Duration {
        todo!()
    }

    pub fn pause(&self) {
        todo!()
    }

    pub fn resume(&self) {
        todo!()
    }

    pub fn is_paused(&self) -> bool {
        todo!()
    }

    /// 请求跳到 `to`。裁决在同步源下一次被拉的时候给出。
    pub fn request_seek(
        &self,
        to: Duration,
    ) -> mpsc::Receiver<Result<(), SeekError>> {
        let _ = to;
        todo!()
    }

    pub fn report(&self) -> Report {
        todo!()
    }
}

/// 同步源本体。
pub struct SyncSource<F: Feed> {
    feed: F,
    shared: Arc<SyncShared>,
    follower: Follower,
    channels: usize,
    rate: f64,
}

impl<F: Feed> SyncSource<F> {
    pub fn new(feed: F, shared: Arc<SyncShared>) -> Self {
        let channels = usize::from(feed.channels().get());
        let rate = f64::from(feed.sample_rate().get());
        Self {
            feed,
            shared,
            follower: Follower::default(),
            channels,
            rate,
        }
    }
}

impl<F: Feed> Iterator for SyncSource<F> {
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        let _ = (&mut self.feed, &self.shared, &mut self.follower, self.channels, self.rate);
        todo!()
    }
}

impl<F: Feed> Source for SyncSource<F> {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        self.feed.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.feed.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }

    fn try_seek(&mut self, to: Duration) -> Result<(), SeekError> {
        let _ = to;
        todo!()
    }
}

#[allow(dead_code)]
fn _silence_unused() {
    let _ = Ordering::Relaxed;
}
