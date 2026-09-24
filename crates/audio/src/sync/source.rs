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

use super::follower::{Decision, Follower, Stats};
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
    seek_pending: AtomicBool,
    seek: Mutex<Option<SeekRequest>>,
    stats: Mutex<Report>,
    /// 最近一块：它第一帧的呈现时刻，与那一帧是媒体的第几纳秒(见 [`SyncShared::pairing`])。
    pairing: Mutex<Option<(i64, i64)>>,
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
        Arc::new(Self {
            target: Mutex::new(Target::Free),
            block_seq: AtomicU64::new(0),
            block_present_ns: AtomicI64::new(0),
            block_frames: AtomicU64::new(0),
            position_ns: AtomicI64::new(0),
            paused: AtomicBool::new(false),
            seek_pending: AtomicBool::new(false),
            seek: Mutex::new(None),
            stats: Mutex::new(Report::default()),
            pairing: Mutex::new(None),
        })
    }

    pub fn set_target(&self, target: Target) {
        *lock(&self.target) = target;
    }

    pub fn target(&self) -> Target {
        *lock(&self.target)
    }

    /// 输出后端每块调一次：这一块第一帧的呈现时刻(本机单调时钟),以及这一块多少帧。
    pub fn block(&self, present_ns: i64, frames: u64) {
        self.block_present_ns
            .store(present_ns, Ordering::Relaxed);
        self.block_frames.store(frames, Ordering::Relaxed);
        // 最后才加块号：读的一方看见新块号时，上面两个已经是这一块的
        self.block_seq.fetch_add(1, Ordering::Release);
    }

    /// 最近出声的那一帧在媒体的哪里。
    pub fn position(&self) -> Duration {
        let ns = self.position_ns.load(Ordering::Relaxed);
        Duration::from_nanos(ns.max(0) as u64)
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// 请求跳到 `to`。裁决在同步源下一次被拉的时候给出。
    pub fn request_seek(
        &self,
        to: Duration,
    ) -> mpsc::Receiver<Result<(), SeekError>> {
        let (tx, rx) = mpsc::channel();
        *lock(&self.seek) = Some((to, tx));
        self.seek_pending.store(true, Ordering::Release);
        rx
    }

    pub fn report(&self) -> Report {
        *lock(&self.stats)
    }

    /// 最近一块第一帧的呈现时刻(本机单调时钟),与那一帧是媒体的第几纳秒。
    ///
    /// 主端把自己的实际播放写成共同计划靠它(#137 ⑤):两个数取自同一块，不会一个新一个旧。
    pub fn pairing(&self) -> Option<(i64, i64)> {
        *lock(&self.pairing)
    }
}

/// 锁中毒了照样拿：里面只有纯数据，前一个持有者 panic 不会让它处于半截状态。
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// 从媒体拉一整帧的结果。
enum Frame {
    Ready,
    Starved,
    End,
}

/// 同步源本体。
pub struct SyncSource<F: Feed> {
    feed: F,
    shared: Arc<SyncShared>,
    follower: Follower,
    channels: usize,
    rate: f64,
    /// 下一帧输出对应媒体的第几帧(可以带小数：速率修正时在两帧之间插值)。
    pos: f64,
    /// 手上从媒体拉出来的帧，`frames[0]` 是媒体第 `front` 帧。
    frames: std::collections::VecDeque<Vec<Sample>>,
    front: i64,
    /// 拉了一半的帧(欠载卡在一帧中间)。
    partial: Vec<Sample>,
    /// 每输出一帧读指针走多少。
    step: f64,
    /// 还在对齐：照常消耗但不出声。
    muted: bool,
    /// 不出声也不消耗：`Some(n)` 还要静音 n 帧然后起播;`None` 整块静音直到下一次决定。
    holding: Option<Option<u64>>,
    seen_block: u64,
    starving: bool,
    ended: bool,
    out: Vec<Sample>,
    out_at: usize,
}

impl<F: Feed> SyncSource<F> {
    pub fn new(feed: F, shared: Arc<SyncShared>) -> Self {
        let channels = usize::from(feed.channels().get());
        let rate = f64::from(feed.sample_rate().get());
        // 换了一路新媒体：上一路的位置、配对与没来得及执行的跳转都不属于它。
        shared.position_ns.store(0, Ordering::Relaxed);
        lock(&shared.pairing).take();
        shared.seek_pending.store(false, Ordering::Relaxed);
        lock(&shared.seek).take();
        Self {
            feed,
            shared,
            follower: Follower::default(),
            channels,
            rate,
            pos: 0.0,
            frames: std::collections::VecDeque::new(),
            front: 0,
            partial: Vec::with_capacity(channels),
            step: 1.0,
            muted: false,
            holding: None,
            seen_block: 0,
            starving: false,
            ended: false,
            out: vec![0.0; channels],
            out_at: channels,
        }
    }

    /// 新的一块来了：按时间线决定这一块怎么放。
    fn on_block(&mut self) {
        let seq = self.shared.block_seq.load(Ordering::Acquire);
        if seq == self.seen_block {
            return;
        }
        self.seen_block = seq;
        let present =
            self.shared.block_present_ns.load(Ordering::Relaxed);
        let frames =
            self.shared.block_frames.load(Ordering::Relaxed);
        let target = self.shared.target();
        let decision = self.follower.decide(
            &target, present, self.pos, self.rate, frames,
        );
        let jumped = self.apply(decision);
        // 起播之前 seek 过去备着的那一下之后，同一块里还要定出前导帧：起播可能就落在这一块。
        // 已经开始的时间线上跳过之后不重新决定，同一个呈现时刻量出来的误差恒为零，不是真的对齐了。
        if jumped && target.desired(present).is_none() {
            let decision = self.follower.decide(
                &target, present, self.pos, self.rate, frames,
            );
            self.apply(decision);
        }
        *lock(&self.shared.pairing) =
            Some((present, (self.pos * 1e9 / self.rate) as i64));
        let mut stats = lock(&self.shared.stats);
        stats.follower = self.follower.stats;
    }

    /// 照决定办。跳过了(丢帧或 seek)返回真。
    fn apply(&mut self, decision: Decision) -> bool {
        match decision {
            Decision::Hold { lead_frames } => {
                self.holding = Some(lead_frames);
                false
            }
            Decision::Play { step, muted } => {
                self.holding = None;
                self.step = step;
                self.muted = muted;
                false
            }
            Decision::Skip { frames } => {
                self.holding = None;
                self.step = 1.0;
                self.muted = true;
                self.pos += frames as f64;
                true
            }
            Decision::Seek { to_frames } => {
                self.holding = None;
                self.step = 1.0;
                self.muted = true;
                let _ = self.seek_to(to_frames);
                true
            }
        }
    }

    /// 让媒体跳到第 `to` 帧(可以带小数)。成了，手上的帧全作废，下一帧就从那里取。
    ///
    /// 媒体跳到 `to` 所在那一帧的**开头**,读指针停在帧内的小数处 —— 手上的起点与读指针
    /// 得按同一种取整算:一个四舍五入、一个向下取整的话，差出的那一帧让下标成负数。
    fn seek_to(&mut self, to: f64) -> Result<(), SeekError> {
        let to = to.max(0.0);
        let frame = to.floor();
        self.feed.seek(Duration::from_secs_f64(frame / self.rate))?;
        self.frames.clear();
        self.partial.clear();
        self.front = frame as i64;
        self.pos = to;
        self.ended = false;
        Ok(())
    }

    /// 从媒体拉一整帧放到手上。
    fn pull_frame(&mut self) -> Frame {
        while self.partial.len() < self.channels {
            match self.feed.pull() {
                Pulled::Sample(sample) => self.partial.push(sample),
                Pulled::Starved => return Frame::Starved,
                Pulled::End => return Frame::End,
            }
        }
        let frame = core::mem::replace(
            &mut self.partial,
            Vec::with_capacity(self.channels),
        );
        self.frames.push_back(frame);
        Frame::Ready
    }

    /// 手上备齐媒体第 `index` 帧(前面用不着的丢掉)。
    fn ensure(&mut self, index: i64) -> Frame {
        while self.front < index {
            if self.frames.pop_front().is_none() {
                // 手上空了：直接从媒体拉出来丢掉(往前丢帧就走这里)
                match self.pull_frame() {
                    Frame::Ready => {
                        self.frames.pop_front();
                    }
                    other => return other,
                }
            }
            self.front += 1;
        }
        while (self.frames.len() as i64) <= index - self.front {
            match self.pull_frame() {
                Frame::Ready => {}
                other => return other,
            }
        }
        Frame::Ready
    }

    /// 算出下一帧输出放进 `out`。媒体真的放完了返回假。
    fn produce(&mut self) -> bool {
        self.on_block();
        self.take_seek_request();

        let silent = |out: &mut Vec<Sample>| out.fill(0.0);

        if self.shared.is_paused()
            && self.shared.target() == Target::Free
        {
            silent(&mut self.out);
            return true;
        }
        match self.holding {
            Some(None) => {
                silent(&mut self.out);
                return true;
            }
            Some(Some(0)) => {
                // 前导帧放完了：这一帧起播
                self.holding = None;
                self.step = 1.0;
                self.muted = false;
            }
            Some(Some(left)) => {
                self.holding = Some(Some(left - 1));
                silent(&mut self.out);
                return true;
            }
            None => {}
        }

        let index = self.pos.floor() as i64;
        let frac = (self.pos - self.pos.floor()) as f32;
        match self.ensure(index) {
            Frame::Ready => {}
            Frame::Starved => {
                self.starve();
                silent(&mut self.out);
                return true;
            }
            Frame::End => {
                self.ended = true;
                return false;
            }
        }
        let need_next = frac > 1e-6;
        if need_next {
            match self.ensure_next(index) {
                Frame::Starved => {
                    self.starve();
                    silent(&mut self.out);
                    return true;
                }
                // 媒体在这里结束：没有下一帧可插，就用这一帧
                Frame::End | Frame::Ready => {}
            }
        }
        self.starving = false;
        let at = (index - self.front) as usize;
        let a = &self.frames[at];
        let b = self.frames.get(at + 1).filter(|_| need_next);
        for (c, out) in self.out.iter_mut().enumerate() {
            *out = match b {
                Some(b) => a[c] * (1.0 - frac) + b[c] * frac,
                None => a[c],
            };
        }
        self.shared.position_ns.store(
            (self.pos * 1e9 / self.rate) as i64,
            Ordering::Relaxed,
        );
        self.pos += self.step;
        let sounding = !self.muted;
        if self.muted {
            silent(&mut self.out);
        }
        lock(&self.shared.stats).sounding = sounding;
        true
    }

    /// 手上备齐第 `index + 1` 帧(插值要用)。
    fn ensure_next(&mut self, index: i64) -> Frame {
        while (self.frames.len() as i64) <= index + 1 - self.front {
            match self.pull_frame() {
                Frame::Ready => {}
                other => return other,
            }
        }
        Frame::Ready
    }

    fn starve(&mut self) {
        if !self.starving {
            self.starving = true;
            let mut stats = lock(&self.shared.stats);
            stats.starves += 1;
            stats.sounding = false;
        }
    }

    /// 外面要求的跳转(不跟时间线时用户拖进度条):在帧边界上执行，裁决回给请求方。
    fn take_seek_request(&mut self) {
        if !self.shared.seek_pending.swap(false, Ordering::Acquire) {
            return;
        }
        let Some((to, verdict)) = lock(&self.shared.seek).take()
        else {
            return;
        };
        let result = self.seek_to(to.as_secs_f64() * self.rate);
        if result.is_ok() {
            self.muted = false;
            self.step = 1.0;
        }
        let _ = verdict.send(result);
    }
}

impl<F: Feed> Iterator for SyncSource<F> {
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        if self.out_at >= self.channels {
            if self.ended || !self.produce() {
                return None;
            }
            self.out_at = 0;
        }
        let sample = self.out[self.out_at];
        self.out_at += 1;
        Some(sample)
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
        let result = self.seek_to(to.as_secs_f64() * self.rate);
        if result.is_ok() {
            self.muted = false;
            self.step = 1.0;
            self.out_at = self.channels;
        }
        result
    }
}
