//! `SyncSource`:包在媒体外面的 rodio 源，吐出去的每一帧都知道是媒体的哪一帧。
//!
//! 位置、跳转、暂停归它，不归 rodio 的 `Player`:
//! - rodio 的位置计数数的是经过它的采样，这一层内部丢帧、seek 它都不知道;
//! - rodio 的暂停与跳转都在它的周期访问里生效(5ms 一次),卡不准一帧。
//!
//! 所以 `Player` 这边始终是「放着」的，出不出声、从媒体哪里取都由这里决定。

use std::sync::atomic::{
    AtomicBool, AtomicI64, AtomicU64, Ordering,
};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{ChannelCount, Sample, SampleRate, Source};

use super::diag::{Diag, Jump, Note, Why};
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
    ///
    /// 在实时的送声线程上调，不许等别的线程(#192)。
    fn seek(
        &mut self,
        to: Duration,
    ) -> Result<(), SeekError>;
    /// 同 [`Feed::seek`],但调用方不在送声线程上，可以花一小会儿等出如实的裁决。
    fn seek_and_wait(
        &mut self,
        to: Duration,
    ) -> Result<(), SeekError> {
        self.seek(to)
    }
    fn channels(&self) -> ChannelCount;
    fn sample_rate(&self) -> SampleRate;
    /// 手上还缓冲着几个采样(观测用，#192)。说不出来就是 `None`。
    fn level(&self) -> Option<usize> {
        None
    }
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

    fn seek(
        &mut self,
        to: Duration,
    ) -> Result<(), SeekError> {
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
type SeekRequest =
    (Duration, mpsc::Sender<Result<(), SeekError>>);

/// 回调线程攒着、还没被持流线程取走的诊断最多这么多条。每类一秒最多一条，持流线程
/// 至多 250ms 醒一次，64 条是好几倍的余量;满了就丢，不等。
const NOTE_BACKLOG: usize = 64;

/// 同步源与外面共用的那一份：时间线、输出后端报来的呈现时刻、当前位置。
///
/// 声卡回调线程与控制线程都碰它。回调那一侧绝不等锁(#192):它是实时线程，等的若是一个被
/// 编译抢走 CPU 的普通线程，就是一下卡顿(优先级反转)。所以回调里只做原子读写与 `try_lock`,
/// 拿不到就用上一次的值或者跳过这一次;日志也不在回调里写，排进 `notes` 由持流线程写。
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
    follower_stats: Mutex<Stats>,
    sounding: AtomicBool,
    starves: AtomicU64,
    /// 输出回调的观测(#192),见 [`SyncShared::callback_block`]。
    callback: CallbackStats,
    notes: mpsc::SyncSender<Note>,
    pending_notes: Mutex<mpsc::Receiver<Note>>,
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
    /// 输出回调填一块最久花过多少纳秒(墙钟)。
    pub callback_worst_ns: u64,
    /// 最久那一块花掉的线程 CPU 时间。与墙钟差不多 = 在算或在自旋;远小于墙钟 = 在等、缺页或被抢占。
    pub callback_worst_cpu_ns: u64,
    /// 填一块花的时间超过这一块本身时长的次数 —— 每一次都必然欠载。
    pub callbacks_late: u64,
    /// 相邻两块开始之间最长隔了多久(回调被叫醒得晚)。
    pub wake_gap_worst_ns: u64,
    /// 回调开始时声卡队列里最少还剩几帧。
    pub queue_low_frames: u64,
    /// 最近一块的帧数(实际协商出的 period)。
    pub period_frames: u64,
    /// 声卡报的欠载(`BufferUnderrun`)次数。
    pub underruns: u64,
    /// 声卡报的别的错误。
    pub other_errors: u64,
    /// 回调拿不到混音器的锁、只好填静音的次数(不算在 `starves` 里)。
    pub fill_misses: u64,
    /// 媒体通道里最少时还剩几个采样(`Feed::level` 报得出来时;报不出是 `u64::MAX`)。
    pub feed_low: u64,
}

/// 输出回调一块的观测，交给 [`SyncShared::callback_block`]。
#[derive(Debug, Clone, Copy)]
pub struct CallbackBlock {
    /// 回调开始的单调时钟。
    pub start_ns: i64,
    pub wall_ns: u64,
    /// 本线程在这一块里用掉的 CPU 时间。
    pub cpu_ns: u64,
    /// 这一块本身的时长。
    pub budget_ns: u64,
    pub frames: u64,
    /// 开始时声卡队列里还剩几帧。
    pub queued_frames: u64,
}

/// 回调线程记下的一件事，按发生顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// 填一块花得比块长还久。
    Late {
        at_ns: i64,
        wall_ns: u64,
        cpu_ns: u64,
    },
    /// 声卡报欠载。
    Underrun { at_ns: i64 },
}

/// 最多记这么多件事，之后的丢掉(满载检查两分钟里该是个位数)。
const EVENT_SLOTS: usize = 64;

/// 输出回调那一侧的计数。全是原子量，回调里随便写;事件记在预先开好的槽里。
#[derive(Debug)]
struct CallbackStats {
    worst_ns: AtomicU64,
    worst_cpu_ns: AtomicU64,
    late: AtomicU64,
    last_start_ns: AtomicI64,
    gap_worst_ns: AtomicU64,
    queue_low: AtomicU64,
    period: AtomicU64,
    underruns: AtomicU64,
    other_errors: AtomicU64,
    fill_misses: AtomicU64,
    feed_low: AtomicU64,
    /// 每件事四个数：种类(1 = Late,2 = Underrun)、时刻、墙钟、CPU。
    events: [[AtomicU64; 4]; EVENT_SLOTS],
    event_count: AtomicU64,
}

impl Default for CallbackStats {
    fn default() -> Self {
        Self {
            worst_ns: AtomicU64::new(0),
            worst_cpu_ns: AtomicU64::new(0),
            late: AtomicU64::new(0),
            last_start_ns: AtomicI64::new(i64::MIN),
            gap_worst_ns: AtomicU64::new(0),
            queue_low: AtomicU64::new(u64::MAX),
            period: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            other_errors: AtomicU64::new(0),
            fill_misses: AtomicU64::new(0),
            feed_low: AtomicU64::new(u64::MAX),
            events: std::array::from_fn(|_| {
                std::array::from_fn(|_| AtomicU64::new(0))
            }),
            event_count: AtomicU64::new(0),
        }
    }
}

impl CallbackStats {
    fn record(
        &self,
        kind: u64,
        at_ns: i64,
        wall: u64,
        cpu: u64,
    ) {
        let slot = self
            .event_count
            .fetch_add(1, Ordering::Relaxed)
            as usize;
        let Some(event) = self.events.get(slot) else {
            return;
        };
        event[1].store(at_ns as u64, Ordering::Relaxed);
        event[2].store(wall, Ordering::Relaxed);
        event[3].store(cpu, Ordering::Relaxed);
        // 种类最后写：读的一方看到非零种类时，另外三个数已经写好
        event[0].store(kind, Ordering::Release);
    }
}

impl SyncShared {
    pub fn new() -> Arc<Self> {
        let (notes, pending_notes) =
            mpsc::sync_channel(NOTE_BACKLOG);
        Arc::new(Self {
            target: Mutex::new(Target::Free),
            block_seq: AtomicU64::new(0),
            block_present_ns: AtomicI64::new(0),
            block_frames: AtomicU64::new(0),
            position_ns: AtomicI64::new(0),
            paused: AtomicBool::new(false),
            seek_pending: AtomicBool::new(false),
            seek: Mutex::new(None),
            follower_stats: Mutex::new(Stats::default()),
            sounding: AtomicBool::new(false),
            starves: AtomicU64::new(0),
            callback: CallbackStats::default(),
            notes,
            pending_notes: Mutex::new(pending_notes),
            pairing: Mutex::new(None),
        })
    }

    pub fn set_target(&self, target: Target) {
        *lock(&self.target) = target;
    }

    pub fn target(&self) -> Target {
        *lock(&self.target)
    }

    /// 回调线程读时间线：锁被占着就给 `None`,调用方沿用上一次读到的。
    fn try_target(&self) -> Option<Target> {
        try_lock(&self.target).map(|target| *target)
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
        Report {
            follower: *lock(&self.follower_stats),
            sounding: self.sounding.load(Ordering::Relaxed),
            starves: self.starves.load(Ordering::Relaxed),
            callback_worst_ns: load(
                &self.callback.worst_ns,
            ),
            callback_worst_cpu_ns: load(
                &self.callback.worst_cpu_ns,
            ),
            callbacks_late: load(&self.callback.late),
            wake_gap_worst_ns: load(
                &self.callback.gap_worst_ns,
            ),
            queue_low_frames: load(
                &self.callback.queue_low,
            ),
            period_frames: load(&self.callback.period),
            underruns: load(&self.callback.underruns),
            other_errors: load(&self.callback.other_errors),
            fill_misses: load(&self.callback.fill_misses),
            feed_low: load(&self.callback.feed_low),
        }
    }

    /// 输出回调每块报一次(#192)。只做原子操作，回调里调得起。满载卡顿时据此分清是回调自己慢、
    /// 叫醒得晚，还是别处。
    pub fn callback_block(&self, block: CallbackBlock) {
        let stats = &self.callback;
        if stats
            .worst_ns
            .fetch_max(block.wall_ns, Ordering::Relaxed)
            < block.wall_ns
        {
            stats
                .worst_cpu_ns
                .store(block.cpu_ns, Ordering::Relaxed);
        }
        if block.wall_ns > block.budget_ns {
            stats.late.fetch_add(1, Ordering::Relaxed);
            stats.record(
                1,
                block.start_ns,
                block.wall_ns,
                block.cpu_ns,
            );
        }
        let last = stats
            .last_start_ns
            .swap(block.start_ns, Ordering::Relaxed);
        if last != i64::MIN {
            stats.gap_worst_ns.fetch_max(
                block.start_ns.saturating_sub(last).max(0)
                    as u64,
                Ordering::Relaxed,
            );
        }
        stats.queue_low.fetch_min(
            block.queued_frames,
            Ordering::Relaxed,
        );
        stats.period.store(block.frames, Ordering::Relaxed);
    }

    /// 声卡报欠载。在回调线程上调。
    pub fn underrun(&self, at_ns: i64) {
        self.callback
            .underruns
            .fetch_add(1, Ordering::Relaxed);
        self.callback.record(2, at_ns, 0, 0);
    }

    /// 声卡报了欠载以外的错误。
    pub fn other_error(&self) {
        self.callback
            .other_errors
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 回调拿不到混音器的锁，这一块填了静音。
    pub fn fill_missed(&self) {
        self.callback
            .fill_misses
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 回调线程记下的事，按发生顺序。
    pub fn events(&self) -> Vec<Event> {
        let count = (load(&self.callback.event_count)
            as usize)
            .min(EVENT_SLOTS);
        self.callback.events[..count]
            .iter()
            .filter_map(|e| {
                let at_ns = load(&e[1]) as i64;
                match e[0].load(Ordering::Acquire) {
                    1 => Some(Event::Late {
                        at_ns,
                        wall_ns: load(&e[2]),
                        cpu_ns: load(&e[3]),
                    }),
                    2 => Some(Event::Underrun { at_ns }),
                    _ => None,
                }
            })
            .collect()
    }

    /// 取走回调线程攒下的诊断，一条一行。持流线程每次醒来调一次，把它们写进日志。
    pub fn take_notes(&self) -> Vec<String> {
        lock(&self.pending_notes)
            .try_iter()
            .map(|note| note.to_string())
            .collect()
    }

    /// 回调线程记一条诊断。队列满了就丢：宁可少一行日志，不让实时线程等。
    fn note(&self, note: Option<Note>) {
        if let Some(note) = note {
            let _ = self.notes.try_send(note);
        }
    }

    /// 此刻用不着声卡(输出后端据此在暂停久了之后关流，#138):不跟时间线时按着暂停，或者跟着
    /// 一条暂停着的时间线。有跳转在等就还用得着 —— 跳转要等声卡来拉才执行。
    pub fn idle(&self) -> bool {
        if self.seek_pending.load(Ordering::Acquire) {
            return false;
        }
        match self.target() {
            Target::Free => self.is_paused(),
            Target::Follow { playing, .. } => !playing,
        }
    }

    /// 输出后端关了流：最近那一块的配对作废(主端恢复时别拿几十秒前的配对写计划),
    /// 也不再算在出声。
    pub fn output_closed(&self) {
        lock(&self.pairing).take();
        self.sounding.store(false, Ordering::Relaxed);
    }

    /// 最近一块第一帧的呈现时刻(本机单调时钟),与那一帧是媒体的第几纳秒。
    ///
    /// 主端把自己的实际播放写成共同计划靠它(#137 ⑤):两个数取自同一块，不会一个新一个旧。
    pub fn pairing(&self) -> Option<(i64, i64)> {
        *lock(&self.pairing)
    }
}

#[cfg(test)]
impl SyncShared {
    /// 测试用：像一个被抢走 CPU 的控制线程那样，把同步状态的每一把锁都拿在手里。
    pub(crate) fn hold_locks(&self) -> impl Sized + '_ {
        (
            lock(&self.target),
            lock(&self.seek),
            lock(&self.follower_stats),
            lock(&self.pairing),
        )
    }
}

/// 锁中毒了照样拿：里面只有纯数据，前一个持有者 panic 不会让它处于半截状态。
fn lock<T>(
    mutex: &Mutex<T>,
) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn load(atomic: &AtomicU64) -> u64 {
    atomic.load(Ordering::Relaxed)
}

/// 回调线程用的那一种：别人拿着就给 `None`,绝不等。中毒照 [`lock`] 的理由照样拿。
fn try_lock<T>(
    mutex: &Mutex<T>,
) -> Option<std::sync::MutexGuard<'_, T>> {
    match mutex.try_lock() {
        Ok(guard) => Some(guard),
        Err(std::sync::TryLockError::Poisoned(e)) => {
            Some(e.into_inner())
        }
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
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
    /// 最近一次读到的时间线。锁被占着的那一刻就沿用它(见 [`SyncShared::try_target`])。
    target: Target,
    follower: Follower,
    channels: usize,
    rate: f64,
    /// 下一帧输出对应媒体的第几帧(可以带小数：速率修正时在两帧之间插值)。
    pos: f64,
    /// 手上从媒体拉出来的帧，`frames[0]` 是媒体第 `front` 帧。
    frames: std::collections::VecDeque<Vec<Sample>>,
    /// 用过的帧缓冲，留着下次装新帧：回调里每帧分配一次的话，满载时还要去抢分配器的锁。
    spare: Vec<Vec<Sample>>,
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
    /// 这一段欠载已经静音了几帧。
    starve_frames: u64,
    diag: Diag,
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
        let target = shared.target();
        Self {
            feed,
            shared,
            target,
            follower: Follower::default(),
            channels,
            rate,
            pos: 0.0,
            frames: std::collections::VecDeque::new(),
            spare: Vec::new(),
            front: 0,
            partial: Vec::with_capacity(channels),
            step: 1.0,
            muted: false,
            holding: None,
            seen_block: 0,
            starving: false,
            starve_frames: 0,
            diag: Diag::default(),
            ended: false,
            out: vec![0.0; channels],
            out_at: channels,
        }
    }

    /// 新的一块来了：按时间线决定这一块怎么放。
    fn on_block(&mut self) {
        let seq =
            self.shared.block_seq.load(Ordering::Acquire);
        if seq == self.seen_block {
            return;
        }
        self.seen_block = seq;
        if let Some(level) = self.feed.level() {
            self.shared
                .callback
                .feed_low
                .fetch_min(level as u64, Ordering::Relaxed);
        }
        let present = self
            .shared
            .block_present_ns
            .load(Ordering::Relaxed);
        let frames = self
            .shared
            .block_frames
            .load(Ordering::Relaxed);
        let target = self.target;
        self.diag.block(match target {
            Target::Follow { anchor, .. } => {
                Some(anchor.media_ns - anchor.at_ns)
            }
            Target::Free => None,
        });
        // 手上的纠正还没做完(断流中、静音等着):这一块「落后/超前一块」是它造成的，不是跑偏，
        // 不再判一次 —— 否则每块都丢一次帧、seek 一次(#145 现场每秒 85 次)。暂停照判;拖进度条
        // 等这次纠正做完再判(静音等完那一段，或断流等到数据回来)。
        if self.follower.running()
            && target.desired(present).is_some()
            && self.correcting()
        {
            self.pair(present);
            return;
        }
        let why = match target.desired(present) {
            None => Why::Hold,
            Some(_) if self.follower.running() => {
                Why::Drift
            }
            Some(_) => Why::Entry,
        };
        let decision = self.follower.decide(
            &target, present, self.pos, self.rate, frames,
        );
        self.note(decision, why);
        let jumped = self.apply(decision);
        // 起播之前 seek 过去备着的那一下之后，同一块里还要定出前导帧：起播可能就落在这一块。
        // 已经开始的时间线上跳过之后不重新决定，同一个呈现时刻量出来的误差恒为零，不是真的对齐了。
        if jumped && target.desired(present).is_none() {
            let decision = self.follower.decide(
                &target, present, self.pos, self.rate,
                frames,
            );
            self.note(decision, Why::Hold);
            self.apply(decision);
        }
        self.pair(present);
    }

    /// 记下这一块的配对与跟随器读数。
    fn pair(&mut self, present: i64) {
        // 锁被占着就跳过这一块：下一块再记，读的一方拿到的是上一块的配对，两个数仍取自同一块
        if let Some(mut pairing) =
            try_lock(&self.shared.pairing)
        {
            *pairing = Some((
                present,
                (self.pos * 1e9 / self.rate) as i64,
            ));
        }
        if let Some(mut stats) =
            try_lock(&self.shared.follower_stats)
        {
            *stats = self.follower.stats;
        }
    }

    /// 有一次纠正还在进行：媒体断着流，或者还在为超前静音等着。
    fn correcting(&self) -> bool {
        self.starving
            || matches!(self.holding, Some(Some(left)) if left > 0)
    }

    /// 跳的决定记一行诊断日志(#145),误差按跳之前的读指针算。
    fn note(&mut self, decision: Decision, why: Why) {
        let ns =
            |frames: f64| (frames * 1e9 / self.rate) as i64;
        let note = match decision {
            Decision::Skip { frames } => self.diag.jump(
                Jump::Skip,
                why,
                -ns(frames as f64),
            ),
            Decision::Seek { to_frames } => self.diag.jump(
                Jump::Seek,
                why,
                ns(self.pos - to_frames),
            ),
            Decision::Wait { frames } => self.diag.jump(
                Jump::Wait,
                why,
                ns(frames as f64),
            ),
            Decision::Hold { .. }
            | Decision::Play { .. } => None,
        };
        self.shared.note(note);
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
                if let Err(e) =
                    self.seek_to(to_frames, F::seek)
                {
                    self.seek_failed(to_frames, &e);
                }
                true
            }
            Decision::Wait { frames } => {
                self.wait(frames);
                true
            }
        }
    }

    /// 超前了：先出 `frames` 帧静音，不消耗媒体，之后照放。
    fn wait(&mut self, frames: u64) {
        self.holding = Some(Some(frames));
        self.step = 1.0;
        self.muted = true;
    }

    /// 跳不动(曲尾解码线程已经收工，只剩缓冲):往前的照样丢帧过去，往回的静音等时间线追上。
    /// 不这么办的话读指针原地不动，下一块又判一次 seek,一直静音到切歌(#145 现场)。
    fn seek_failed(&mut self, to: f64, e: &SeekError) {
        self.shared.note(Some(Note::SeekFailed {
            error: e.to_string(),
            backwards: to < self.pos,
        }));
        if to < self.pos {
            self.wait((self.pos - to).round() as u64);
        } else {
            self.pos = to;
        }
    }

    /// 让媒体跳到第 `to` 帧(可以带小数)。成了，手上的帧全作废，下一帧就从那里取。
    ///
    /// 媒体跳到 `to` 所在那一帧的**开头**,读指针停在帧内的小数处 —— 手上的起点与读指针
    /// 得按同一种取整算:一个四舍五入、一个向下取整的话，差出的那一帧让下标成负数。
    ///
    /// `seek` 是媒体那一侧怎么跳：送声线程上用 [`Feed::seek`],别处可以用 [`Feed::seek_and_wait`]。
    fn seek_to(
        &mut self,
        to: f64,
        seek: fn(&mut F, Duration) -> Result<(), SeekError>,
    ) -> Result<(), SeekError> {
        let to = to.max(0.0);
        let frame = to.floor();
        seek(
            &mut self.feed,
            Duration::from_secs_f64(frame / self.rate),
        )?;
        self.recycle_all();
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
                Pulled::Sample(sample) => {
                    self.partial.push(sample)
                }
                Pulled::Starved => return Frame::Starved,
                Pulled::End => return Frame::End,
            }
        }
        let fresh = self.spare.pop().unwrap_or_else(|| {
            Vec::with_capacity(self.channels)
        });
        let frame =
            core::mem::replace(&mut self.partial, fresh);
        self.frames.push_back(frame);
        Frame::Ready
    }

    /// 丢掉手上最前面那一帧，缓冲留着下次用。手上没有帧返回假。
    fn recycle_front(&mut self) -> bool {
        let Some(mut frame) = self.frames.pop_front()
        else {
            return false;
        };
        frame.clear();
        self.spare.push(frame);
        true
    }

    /// 手上的帧全作废，缓冲留着下次用。
    fn recycle_all(&mut self) {
        while self.recycle_front() {}
    }

    /// 手上备齐媒体第 `index` 帧(前面用不着的丢掉)。
    fn ensure(&mut self, index: i64) -> Frame {
        while self.front < index {
            if !self.recycle_front() {
                // 手上空了：直接从媒体拉出来丢掉(往前丢帧就走这里)
                match self.pull_frame() {
                    Frame::Ready => {
                        self.recycle_front();
                    }
                    other => return other,
                }
            }
            self.front += 1;
        }
        while (self.frames.len() as i64)
            <= index - self.front
        {
            match self.pull_frame() {
                Frame::Ready => {}
                other => return other,
            }
        }
        Frame::Ready
    }

    /// 算出下一帧输出放进 `out`。媒体真的放完了返回假。
    fn produce(&mut self) -> bool {
        if let Some(target) = self.shared.try_target() {
            self.target = target;
        }
        self.on_block();
        self.take_seek_request();

        let silent = |out: &mut Vec<Sample>| out.fill(0.0);

        if self.shared.is_paused()
            && self.target == Target::Free
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
        if self.starving {
            self.starving = false;
            let note = self.diag.starved(
                (self.starve_frames as f64 * 1e9
                    / self.rate) as i64,
                (self.pos * 1e9 / self.rate) as i64,
                self.muted,
            );
            self.shared.note(note);
            self.starve_frames = 0;
        }
        let at = (index - self.front) as usize;
        let a = &self.frames[at];
        let b =
            self.frames.get(at + 1).filter(|_| need_next);
        for (c, out) in self.out.iter_mut().enumerate() {
            *out = match b {
                Some(b) => {
                    a[c] * (1.0 - frac) + b[c] * frac
                }
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
        self.shared
            .sounding
            .store(sounding, Ordering::Relaxed);
        true
    }

    /// 手上备齐第 `index + 1` 帧(插值要用)。
    fn ensure_next(&mut self, index: i64) -> Frame {
        while (self.frames.len() as i64)
            <= index + 1 - self.front
        {
            match self.pull_frame() {
                Frame::Ready => {}
                other => return other,
            }
        }
        Frame::Ready
    }

    fn starve(&mut self) {
        self.starve_frames += 1;
        if !self.starving {
            self.starving = true;
            // 跟着时间线时，数据回来那一刻多半已经错位：静音到下一块判过再出声。
            if self.follower.running() {
                self.muted = true;
            }
            self.shared
                .starves
                .fetch_add(1, Ordering::Relaxed);
            self.shared
                .sounding
                .store(false, Ordering::Relaxed);
        }
    }

    /// 外面要求的跳转(不跟时间线时用户拖进度条):在帧边界上执行，裁决回给请求方。
    fn take_seek_request(&mut self) {
        if !self
            .shared
            .seek_pending
            .swap(false, Ordering::Acquire)
        {
            return;
        }
        let Some(mut request) = try_lock(&self.shared.seek)
        else {
            // 请求方正拿着锁(刚放进请求):留着，下一帧再来取
            self.shared
                .seek_pending
                .store(true, Ordering::Release);
            return;
        };
        let Some((to, verdict)) = request.take() else {
            return;
        };
        drop(request);
        let result = self
            .seek_to(to.as_secs_f64() * self.rate, F::seek);
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

    /// 不在送声线程上调(交给 rodio 之前，见 `Player::play_from`),所以等得起媒体的裁决。
    fn try_seek(
        &mut self,
        to: Duration,
    ) -> Result<(), SeekError> {
        let result = self.seek_to(
            to.as_secs_f64() * self.rate,
            F::seek_and_wait,
        );
        if result.is_ok() {
            self.muted = false;
            self.step = 1.0;
            self.out_at = self.channels;
        }
        result
    }
}
