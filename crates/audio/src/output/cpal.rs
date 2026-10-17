//! 桌面输出：cpal。呈现时刻 = 回调那一刻的单调时钟 + cpal 给出的 `playback − callback`
//! (ALSA `snd_pcm_status` 的 delay;PipeWire 桌面上走的是 pipewire-alsa 插件)。

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use ::cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::mixer::Mixer;
use rodio::{ChannelCount, SampleRate};

use super::{
    SharedMixer, Signal, fill, promotion_warning, watch,
};
use crate::AudioError;
use crate::clock::monotonic_ns;
use crate::sync::SyncShared;

/// 开流后最多等这么久，等回调线程报上它的调度策略。一块缓冲十几到几十毫秒，这是它的十几倍。
const POLICY_WAIT: Duration = Duration::from_millis(500);

/// 回调线程还没报调度策略。
const POLICY_UNKNOWN: i32 = -1;

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
    let source = source.clone();
    let shared = shared.clone();
    let broken = broken.clone();
    // cpal 升实时失败只往 stderr 打一行，日志里看不见;所以由回调线程自己报一次策略(#192)
    let policy = Arc::new(AtomicI32::new(POLICY_UNKNOWN));
    let seen = policy.clone();
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], info: &::cpal::OutputCallbackInfo| {
                if seen.load(Ordering::Relaxed) == POLICY_UNKNOWN {
                    // SAFETY: 只读本线程的调度策略，没有指针参数
                    let own = unsafe { libc::sched_getscheduler(0) };
                    seen.store(own, Ordering::Relaxed);
                }
                let now = monotonic_ns();
                let ts = info.timestamp();
                // 算不出 delay(时钟倒退之类)就当 0:宁可让同步层看见一个偏小的延迟，
                // 也不在回调里报错。
                let delay = ts
                    .playback
                    .duration_since(&ts.callback)
                    .map_or(0, |d| d.as_nanos() as i64);
                shared.block(now + delay, data.len() as u64 / channels);
                fill(&source, data);
            },
            move |error| {
                log::warn!("cpal 输出出错: {error}");
                // 欠载/超载(xrun)ALSA 自己会恢复，流还在走;当成断了去重开反倒让声音断一截、
                // 呈现时刻从头估。只有设备没了、或者流的配置作废了才重开。
                if matches!(
                    error,
                    ::cpal::StreamError::DeviceNotAvailable
                        | ::cpal::StreamError::StreamInvalidated
                ) {
                    broken.store(true, Ordering::Relaxed);
                }
            },
            None,
        )
        .map_err(|e| AudioError::Device(e.to_string()))?;
    stream
        .play()
        .map_err(|e| AudioError::Device(e.to_string()))?;
    check_promotion(&policy);
    Ok(stream)
}

/// 等回调线程报上调度策略，不是实时就警告一行。播放照常继续：升不上去只是满载时会卡。
fn check_promotion(policy: &AtomicI32) {
    let deadline = Instant::now() + POLICY_WAIT;
    let mut seen = policy.load(Ordering::Relaxed);
    while seen == POLICY_UNKNOWN
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
        seen = policy.load(Ordering::Relaxed);
    }
    let seen = (seen >= 0).then_some(seen);
    match promotion_warning(seen) {
        Some(warning) => log::warn!("{warning}"),
        None => log::info!("送声线程已是实时优先级"),
    }
}
