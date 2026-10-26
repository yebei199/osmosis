//! 桌面输出：cpal。呈现时刻 = 回调那一刻的单调时钟 + cpal 给出的 `playback − callback`
//! (ALSA `snd_pcm_status` 的 delay;PipeWire 桌面上走的是 pipewire-alsa 插件)。

use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicU64, Ordering,
};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use ::cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::mixer::Mixer;
use rodio::{ChannelCount, SampleRate};

use super::{
    SharedMixer, Signal, fill, promotion_outcome,
    wait_for_tid, watch,
};
use crate::AudioError;
use crate::clock::monotonic_ns;
use crate::sync::{CallbackBlock, SyncShared};

/// 开流后最多等这么久，等回调线程报上它的线程号。一块缓冲十几到几十毫秒，这是它的十几倍。
const TID_WAIT: Duration = Duration::from_millis(500);

/// 回调线程还没报线程号。
const TID_UNKNOWN: i32 = 0;

/// 错误回调的连续错误状态。
#[derive(Default)]
struct ErrorBurst {
    /// 这一串错误的起始时刻和期间最后见到的成功块号。
    since: i64,
    block: u64,
    /// 饱和计数避免忙循环溢出。
    count: u32,
}

/// 一秒内八次且没有成功数据块，远高于偶发错误的密度。
const BACKEND_ERROR_LIMIT: u32 = 8;
/// 超过一秒的错误不累计为同一串。
const BACKEND_ERROR_WINDOW_NS: i64 = 1_000_000_000;

impl ErrorBurst {
    /// 和真实 cpal 回调共用的错误入口；成功块编号用于打断错误串。
    fn record(
        &mut self,
        error: ::cpal::StreamError,
        shared: &SyncShared,
        broken: &AtomicBool,
        now: i64,
        block: u64,
    ) {
        if matches!(
            error,
            ::cpal::StreamError::BufferUnderrun
        ) {
            self.count = 0;
            shared.underrun(now);
            return;
        }
        shared.other_error();
        if matches!(
            error,
            ::cpal::StreamError::DeviceNotAvailable
                | ::cpal::StreamError::StreamInvalidated
        ) {
            broken.store(true, Ordering::Relaxed);
            return;
        }
        if matches!(
            error,
            ::cpal::StreamError::BackendSpecific { .. }
        ) {
            if self.count == 0
                || block != self.block
                || now.saturating_sub(self.since)
                    >= BACKEND_ERROR_WINDOW_NS
            {
                self.since = now;
                self.block = block;
                self.count = 0;
            }
            self.count = self.count.saturating_add(1);
            if self.count >= BACKEND_ERROR_LIMIT {
                broken.store(true, Ordering::Relaxed);
            }
        }
    }
}

/// 开流、交出混音器，然后守着流直到被叫停。
pub(super) fn run(
    shared: Arc<SyncShared>,
    ready: &mpsc::Sender<Result<Mixer, AudioError>>,
    signals: &mpsc::Receiver<Signal>,
) {
    let device = match default_device() {
        Ok(device) => device,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let config = match device.default_output_config() {
        Ok(config) => config,
        Err(error) => {
            let _ = ready.send(Err(AudioError::Device(
                error.to_string(),
            )));
            return;
        }
    };
    let (Some(channels), Some(rate)) = (
        ChannelCount::new(config.channels()),
        SampleRate::new(config.sample_rate()),
    ) else {
        let _ = ready.send(Err(AudioError::Device(
            "声卡报了 0 声道或 0Hz".to_owned(),
        )));
        return;
    };
    let (mixer, source) =
        rodio::mixer::mixer(channels, rate);
    let source: SharedMixer = Arc::new(Mutex::new(source));
    let broken = Arc::new(AtomicBool::new(false));

    let stream = match start(
        &device,
        &config.into(),
        &source,
        &shared,
        &broken,
    ) {
        Ok(stream) => stream,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(mixer));

    watch(
        signals,
        &shared,
        &broken,
        Some(stream),
        || {
            let device = default_device()?;
            let config =
                device.default_output_config().map_err(
                    |e| AudioError::Device(e.to_string()),
                )?;
            start(
                &device,
                &config.into(),
                &source,
                &shared,
                &broken,
            )
        },
        |elapsed| {
            let frames = (elapsed.as_secs_f64()
                * f64::from(rate.get()))
                as usize;
            fill(
                &source,
                &mut vec![
                    0.0;
                    frames
                        * usize::from(channels.get())
                ],
            );
        },
    );
}

fn default_device() -> Result<::cpal::Device, AudioError> {
    ::cpal::default_host()
        .default_output_device()
        .ok_or_else(|| {
            AudioError::Device(
                "没有默认输出设备".to_owned(),
            )
        })
}

fn start(
    device: &::cpal::Device,
    config: &::cpal::StreamConfig,
    source: &SharedMixer,
    shared: &Arc<SyncShared>,
    broken: &Arc<AtomicBool>,
) -> Result<::cpal::Stream, AudioError> {
    let channels = u64::from(config.channels.max(1));
    let rate = u64::from(config.sample_rate.max(1));
    let source = source.clone();
    let shared = shared.clone();
    let errors = shared.clone();
    let broken = broken.clone();
    // 回调线程第一块报出自己的线程号，持流线程据此替它升实时、查策略(#192)
    let tid = Arc::new(AtomicI32::new(TID_UNKNOWN));
    let reported = tid.clone();
    // 数据和错误回调可能在不同线程；成功块号只负责打断连续错误。
    let blocks = Arc::new(AtomicU64::new(0));
    let successful = blocks.clone();
    let mut burst = ErrorBurst::default();
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], info: &::cpal::OutputCallbackInfo| {
                if reported.load(Ordering::Relaxed) == TID_UNKNOWN {
                    // SAFETY: 无参数，只读本线程的线程号;只在第一块调一次
                    let own = unsafe { libc::gettid() };
                    reported.store(own, Ordering::Relaxed);
                }
                let now = monotonic_ns();
                let cpu = thread_cpu_ns();
                let ts = info.timestamp();
                // 算不出 delay(时钟倒退之类)就当 0:宁可让同步层看见一个偏小的延迟，
                // 也不在回调里报错。
                let delay = ts
                    .playback
                    .duration_since(&ts.callback)
                    .map_or(0, |d| d.as_nanos() as i64);
                let frames = data.len() as u64 / channels;
                shared.block(now + delay, frames);
                if !fill(&source, data) {
                    shared.fill_missed();
                }
                shared.callback_block(CallbackBlock {
                    start_ns: now,
                    wall_ns: monotonic_ns().saturating_sub(now) as u64,
                    cpu_ns: thread_cpu_ns().saturating_sub(cpu),
                    budget_ns: frames * 1_000_000_000 / rate,
                    frames,
                    queued_frames: delay.max(0) as u64 * rate
                        / 1_000_000_000,
                });
                successful.fetch_add(1, Ordering::Relaxed);
            },
            move |error| {
                // 回调只计数和标坏，日志由持流线程按窗汇总。
                burst.record(error, &errors, &broken, monotonic_ns(), blocks.load(Ordering::Relaxed));
            },
            None,
        )
        .map_err(|e| AudioError::Device(e.to_string()))?;
    stream
        .play()
        .map_err(|e| AudioError::Device(e.to_string()))?;
    promote(&tid);
    Ok(stream)
}

/// 本线程到此刻用掉的 CPU 时间(纳秒)。读不出来给 0。
fn thread_cpu_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: 传进去的是栈上一个完整初始化的 timespec
    let rc = unsafe {
        libc::clock_gettime(
            libc::CLOCK_THREAD_CPUTIME_ID,
            &mut ts,
        )
    };
    if rc != 0 {
        return 0;
    }
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// 等回调线程报上线程号，经 rtkit 把它升成实时，再查一次策略;哪一步不成就警告一行。
/// 播放照常继续：升不上去只是满载时会卡。每开一次流做一次 —— 重开的流是新线程。
fn promote(tid: &AtomicI32) {
    match promotion_outcome(
        wait_for_tid(tid, TID_WAIT),
        promote_thread,
        scheduling_policy,
    ) {
        Ok(line) => log::info!("{line}"),
        Err(line) => log::warn!("{line}"),
    }
}

/// 请 rtkit 把线程 `tid` 升成实时。
#[cfg(target_os = "linux")]
fn promote_thread(tid: i32) -> Result<(), String> {
    use super::rtkit;
    rtkit::SystemRtkit::connect().and_then(|rtkit| {
        rtkit::promote(&rtkit, tid as u64)
    })
}

/// 别的平台没有 rtkit,只看策略。
#[cfg(not(target_os = "linux"))]
fn promote_thread(_: i32) -> Result<(), String> {
    Ok(())
}

/// 线程 `tid` 此刻的调度策略，读不出是负数。
fn scheduling_policy(tid: i32) -> i32 {
    // SAFETY: 只读该线程的调度策略，没有指针参数
    unsafe { libc::sched_getscheduler(tid) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use similar_asserts::assert_eq;

    /// 使用真正的 cpal 后端错误类型复现 POLLERR 回调。
    fn pollerr() -> ::cpal::StreamError {
        ::cpal::StreamError::BackendSpecific {
            err: ::cpal::BackendSpecificError {
                description:
                    "alsa::poll() returned POLLERR".into(),
            },
        }
    }

    /// 同一秒内持续报错必须标坏；偶发单次和 xrun 保持现有行为。
    #[test]
    fn sustained_backend_errors_break_the_stream_but_one_error_and_xruns_do_not()
     {
        let shared = SyncShared::new();
        let broken = AtomicBool::new(false);
        let mut burst = ErrorBurst::default();
        burst.record(pollerr(), &shared, &broken, 0, 0);
        assert!(!broken.load(Ordering::Relaxed));
        for n in 1..8 {
            burst.record(
                pollerr(),
                &shared,
                &broken,
                n * 1_000_000,
                0,
            );
        }
        assert!(
            broken.load(Ordering::Relaxed),
            "POLLERR 回调忙循环没有请求重开"
        );
        assert_eq!(shared.report().other_errors, 8);

        broken.store(false, Ordering::Relaxed);
        for _ in 0..100 {
            burst.record(
                ::cpal::StreamError::BufferUnderrun,
                &shared,
                &broken,
                10_000_000,
                0,
            );
        }
        assert!(!broken.load(Ordering::Relaxed));
        assert_eq!(shared.report().underruns, 100);
        assert_eq!(shared.report().other_errors, 8);
    }

    /// 超出时间窗、成功数据块和 xrun 都打断连续后端错误，避免无故切流。
    #[test]
    fn separated_backend_errors_never_accumulate_into_a_disconnect()
     {
        let shared = SyncShared::new();
        let broken = AtomicBool::new(false);
        let mut burst = ErrorBurst::default();
        for n in 0..20 {
            burst.record(
                pollerr(),
                &shared,
                &broken,
                n * 1_000_000_000,
                0,
            );
        }
        assert!(!broken.load(Ordering::Relaxed));
        for block in 1..20 {
            for _ in 0..7 {
                burst.record(
                    pollerr(),
                    &shared,
                    &broken,
                    21_000_000_000,
                    block,
                );
            }
        }
        assert!(!broken.load(Ordering::Relaxed));
        for _ in 0..20 {
            burst.record(
                ::cpal::StreamError::BufferUnderrun,
                &shared,
                &broken,
                22_000_000_000,
                20,
            );
            for _ in 0..7 {
                burst.record(
                    pollerr(),
                    &shared,
                    &broken,
                    22_000_000_000,
                    20,
                );
            }
        }
        assert!(!broken.load(Ordering::Relaxed));
    }

    /// 设备或配置失效沿用立即重开的策略。
    #[test]
    fn invalid_devices_still_request_immediate_reopen() {
        for error in [
            ::cpal::StreamError::DeviceNotAvailable,
            ::cpal::StreamError::StreamInvalidated,
        ] {
            let shared = SyncShared::new();
            let broken = AtomicBool::new(false);
            ErrorBurst::default()
                .record(error, &shared, &broken, 0, 0);
            assert!(broken.load(Ordering::Relaxed));
        }
    }

    /// 真回调入口驱动真 watch；只替换声卡流，重开后同一个 mixer 继续产生 PCM，组锚点不动。
    #[test]
    fn pollerr_reopens_after_dropping_the_old_stream_and_preserves_pcm_and_anchor()
     {
        use crate::sync::{Anchor, Target};
        use rodio::buffer::SamplesBuffer;

        /// drop 次数证明开新流之前旧流已经释放。
        struct Stream(Arc<std::sync::atomic::AtomicU64>);
        impl Drop for Stream {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let shared = SyncShared::new();
        let target = Target::Follow {
            anchor: Anchor {
                at_ns: 42,
                media_ns: 9_000_000_000,
            },
            playing: true,
            start_ns: 0,
        };
        shared.set_target(target);
        let (mixer, source) = rodio::mixer::mixer(
            ChannelCount::new(2).expect("stereo"),
            SampleRate::new(48000).expect("rate"),
        );
        mixer.add(SamplesBuffer::new(
            ChannelCount::new(2).expect("stereo"),
            SampleRate::new(48000).expect("rate"),
            vec![0.5; 48000],
        ));
        let source = Arc::new(Mutex::new(source));
        let broken = AtomicBool::new(false);
        let mut burst = ErrorBurst::default();
        for n in 0..8 {
            burst.record(pollerr(), &shared, &broken, n, 0);
        }
        let drops =
            Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (tx, rx) = mpsc::channel();
        tx.send(Signal::Poke).expect("poke");
        // 即使修复退化也会在有界时间退出，不挂住整组测试。
        let stop = tx.clone();
        let timeout = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(1));
            let _ = stop.send(Signal::Stop);
        });
        let mut pcm = [0.0; 128];
        let mut opened = false;
        watch(
            &rx,
            &shared,
            &broken,
            Some(Stream(drops.clone())),
            || {
                assert_eq!(
                    drops.load(Ordering::Relaxed),
                    1
                );
                assert!(fill(&source, &mut pcm));
                shared.block(monotonic_ns(), 64);
                opened = true;
                tx.send(Signal::Stop).expect("stop");
                Ok(Stream(drops.clone()))
            },
            |_| panic!("播放中不能空拉"),
        );
        timeout.join().expect("timer");
        assert!(opened, "持续 POLLERR 后没有重开");
        assert!(pcm.iter().any(|sample| *sample != 0.0));
        assert_eq!(shared.target(), target);
    }
}
