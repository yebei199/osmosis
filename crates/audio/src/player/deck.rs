//! 接在混音器上的唯一一路源：换源、清空、音量。见 `README.md`。
//!
//! 两头：[`Deck`] 在界面线程上，[`DeckSource`] 交给混音器、在送声回调里被拉。回调那一侧
//! 绝不等锁(#192):换源走一个 `try_lock` 的槽，音量与「放空了没有」是原子量，换下来的旧源
//! 经有界通道送回 [`Deck`] 释放。

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, TryLockError, mpsc};
use std::time::Duration;

use rodio::{ChannelCount, Sample, SampleRate, Source};

use crate::pcm::{OUTPUT_CHANNELS, OUTPUT_SAMPLE_RATE};

type Boxed = Box<dyn Source + Send>;

/// 回调换下来、还没被界面线程收走的旧源最多这么多个。换歌是人按出来的，几个就够;
/// 满了就只好在回调里就地释放。
const RETIRED_BACKLOG: usize = 8;

/// 界面线程交给回调的一次换源。
enum Change {
    Play(Boxed),
    Clear,
}

/// 两头共用的那一份。
struct Shared {
    /// 最新的一次换源。界面线程覆盖写(只有最后一次算数),回调 `try_lock` 取走。
    next: Mutex<Option<Change>>,
    pending: AtomicBool,
    /// 音量，`f32` 的位。
    volume: AtomicU32,
    /// 手上没有在放的源(清空了，或者放完了)。
    empty: AtomicBool,
}

/// 界面线程这一头。
pub(crate) struct Deck {
    shared: Arc<Shared>,
    retired: Mutex<mpsc::Receiver<Boxed>>,
}

/// 混音器那一头，在送声回调里被拉。
pub(crate) struct DeckSource {
    shared: Arc<Shared>,
    current: Option<Boxed>,
    retired: mpsc::SyncSender<Boxed>,
}

/// 建一对。`DeckSource` 交给混音器，`Deck` 留在 `Player` 里。
pub(crate) fn deck() -> (Deck, DeckSource) {
    let shared = Arc::new(Shared {
        next: Mutex::new(None),
        pending: AtomicBool::new(false),
        volume: AtomicU32::new(1.0_f32.to_bits()),
        empty: AtomicBool::new(true),
    });
    let (retired, collected) =
        mpsc::sync_channel(RETIRED_BACKLOG);
    (
        Deck {
            shared: shared.clone(),
            retired: Mutex::new(collected),
        },
        DeckSource {
            shared,
            current: None,
            retired,
        },
    )
}

impl Deck {
    /// 改放这一路，替掉正在放的。下一次回调拉的时候生效。
    pub(crate) fn play<S>(&self, source: S)
    where
        S: Source + Send + 'static,
    {
        self.shared.empty.store(false, Ordering::Relaxed);
        self.post(Change::Play(Box::new(
            crate::pcm::normalize(source),
        )));
    }

    /// 停掉正在放的。
    pub(crate) fn clear(&self) {
        self.shared.empty.store(true, Ordering::Relaxed);
        self.post(Change::Clear);
    }

    /// 手上没有在放的：清空过，或者放完了。
    pub(crate) fn empty(&self) -> bool {
        self.collect();
        self.shared.empty.load(Ordering::Relaxed)
    }

    pub(crate) fn volume(&self) -> f32 {
        f32::from_bits(
            self.shared.volume.load(Ordering::Relaxed),
        )
    }

    pub(crate) fn set_volume(&self, volume: f32) {
        self.shared
            .volume
            .store(volume.to_bits(), Ordering::Relaxed);
    }

    fn post(&self, change: Change) {
        // 被覆盖掉的那一次(回调还没来得及取)在这里、界面线程上释放
        let _replaced = self
            .shared
            .next
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(change);
        self.shared.pending.store(true, Ordering::Release);
        self.collect();
    }

    /// 释放回调换下来的旧源。
    fn collect(&self) {
        if let Ok(retired) = self.retired.try_lock() {
            for old in retired.try_iter() {
                drop(old);
            }
        }
    }

    /// 测试用：像一个被抢走 CPU 的界面线程那样拿着换源的槽。
    #[cfg(test)]
    pub(crate) fn hold_slot(&self) -> impl Sized + '_ {
        self.shared.next.lock().unwrap()
    }

    /// 测试用：等一会儿，把回调换下来的旧源收掉。
    #[cfg(test)]
    pub(crate) fn collect_for_test(&self, wait: Duration) {
        std::thread::sleep(wait);
        self.collect();
    }
}

impl DeckSource {
    /// 看一眼有没有换源。槽被界面线程拿着就下一个采样再看。
    fn take_change(&mut self) {
        if !self.shared.pending.load(Ordering::Acquire) {
            return;
        }
        let change = match self.shared.next.try_lock() {
            Ok(mut next) => next.take(),
            Err(TryLockError::Poisoned(e)) => {
                e.into_inner().take()
            }
            Err(TryLockError::WouldBlock) => return,
        };
        self.shared.pending.store(false, Ordering::Release);
        let fresh = match change {
            Some(Change::Play(source)) => Some(source),
            Some(Change::Clear) => None,
            None => return,
        };
        let old =
            core::mem::replace(&mut self.current, fresh);
        self.retire(old);
    }

    /// 旧源送回界面线程释放;通道满了才在这里就地释放。
    fn retire(&self, old: Option<Boxed>) {
        if let Some(old) = old {
            let _ = self.retired.try_send(old);
        }
    }
}

impl Iterator for DeckSource {
    type Item = Sample;

    /// 永不返回 `None`:没东西放就给静音 —— 混音器会把结束了的源摘掉，之后再换源就没人拉了。
    fn next(&mut self) -> Option<Sample> {
        self.take_change();
        let Some(current) = self.current.as_mut() else {
            return Some(0.0);
        };
        match current.next() {
            Some(sample) => Some(
                sample
                    * f32::from_bits(
                        self.shared
                            .volume
                            .load(Ordering::Relaxed),
                    ),
            ),
            None => {
                self.shared
                    .empty
                    .store(true, Ordering::Relaxed);
                let ended = self.current.take();
                self.retire(ended);
                Some(0.0)
            }
        }
    }
}

impl Source for DeckSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    /// 进来的每一路都先统一成这个格式(见 [`Deck::play`]),换源时混音器不必重新协商。
    fn channels(&self) -> ChannelCount {
        ChannelCount::new(OUTPUT_CHANNELS)
            .expect("声道数是编译期常量，非零")
    }

    fn sample_rate(&self) -> SampleRate {
        SampleRate::new(OUTPUT_SAMPLE_RATE)
            .expect("采样率是编译期常量，非零")
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}
