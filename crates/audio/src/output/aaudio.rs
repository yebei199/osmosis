//! 安卓输出：直接用 NDK AAudio。
//!
//! 呈现时刻取自 `AAudioStream_getTimestamp(CLOCK_MONOTONIC)`:它给出「第 P 帧在 T 时刻出声」,
//! 这一块第一帧(本端累计写到第 W 帧)的出声时刻就是 T + (W − P) / 采样率。
//! 流刚起来那几块拿不到时间戳，退回「现在 + 缓冲长度」估算。
//!
//! 采样率钉在 48kHz:路由换了(插拔耳机、连蓝牙)流要重开，重开后混音器的采样率不能变，
//! 设备本身不是 48kHz 时由 AAudio 自己重采样。

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use ndk::audio::{
    AudioCallbackResult, AudioDirection, AudioFormat,
    AudioPerformanceMode, AudioStream, AudioStreamBuilder,
    Clockid,
};
use rodio::mixer::Mixer;
use rodio::{ChannelCount, SampleRate};

use super::{SharedMixer, fill, watch};
use crate::AudioError;
use crate::clock::monotonic_ns;
use crate::pcm::{OUTPUT_CHANNELS, OUTPUT_SAMPLE_RATE};
use crate::sync::SyncShared;

/// 开流、交出混音器，然后守着流直到被叫停。
pub(super) fn run(
    shared: Arc<SyncShared>,
    ready: &mpsc::Sender<Result<Mixer, AudioError>>,
    stop: &mpsc::Receiver<()>,
) {
    let (Some(channels), Some(rate)) = (
        ChannelCount::new(OUTPUT_CHANNELS),
        SampleRate::new(OUTPUT_SAMPLE_RATE),
    ) else {
        let _ = ready.send(Err(AudioError::Device(
            "输出格式是编译期常量，不该为零".to_owned(),
        )));
        return;
    };
    let (mixer, source) = rodio::mixer::mixer(channels, rate);
    let source: SharedMixer = Arc::new(Mutex::new(source));
    let broken = Arc::new(AtomicBool::new(false));

    let mut stream = match start(&source, &shared, &broken) {
        Ok(stream) => stream,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(mixer));

    watch(stop, &|| broken.load(Ordering::Relaxed), || {
        log::warn!("AAudio 输出流断了(换了路由?),重开");
        broken.store(false, Ordering::Relaxed);
        let _ = stream.request_stop();
        match start(&source, &shared, &broken) {
            Ok(fresh) => stream = fresh,
            Err(error) => {
                log::warn!("重开 AAudio 输出失败，稍后再试: {error}");
                broken.store(true, Ordering::Relaxed);
            }
        }
    });
    let _ = stream.request_stop();
}

fn start(
    source: &SharedMixer,
    shared: &Arc<SyncShared>,
    broken: &Arc<AtomicBool>,
) -> Result<AudioStream, AudioError> {
    let channels = i32::from(OUTPUT_CHANNELS);
    let source = source.clone();
    let shared = shared.clone();
    let flag = broken.clone();
    let mut written: i64 = 0;
    let device = |e: ndk::audio::AudioError| AudioError::Device(e.to_string());
    let stream = AudioStreamBuilder::new()
        .map_err(device)?
        .direction(AudioDirection::Output)
        .format(AudioFormat::PCM_Float)
        .channel_count(channels)
        .sample_rate(OUTPUT_SAMPLE_RATE as i32)
        .performance_mode(AudioPerformanceMode::LowLatency)
        .data_callback(Box::new(
            move |stream: &AudioStream, data: *mut c_void, frames: i32| {
                let rate = i64::from(stream.sample_rate().max(1));
                let present = match stream.timestamp(Clockid::Monotonic) {
                    Ok(ts) => {
                        ts.time_nanoseconds
                            + (written - ts.frame_position) * 1_000_000_000 / rate
                    }
                    Err(_) => {
                        monotonic_ns()
                            + i64::from(stream.buffer_size_in_frames()) * 1_000_000_000
                                / rate
                    }
                };
                shared.block(present, frames.max(0) as u64);
                // SAFETY: AAudio 保证 data 指向 frames × 声道数 个 f32(格式是 PCM_Float)。
                let out = unsafe {
                    std::slice::from_raw_parts_mut(
                        data.cast::<f32>(),
                        (frames.max(0) * channels) as usize,
                    )
                };
                fill(&source, out);
                written += i64::from(frames.max(0));
                AudioCallbackResult::Continue
            },
        ))
        // 错误回调里不许重开流(AAudio 的规矩):只置位，由持流线程去重开。
        .error_callback(Box::new(move |_stream, error| {
            log::warn!("AAudio 输出出错: {error:?}");
            flag.store(true, Ordering::Relaxed);
        }))
        .open_stream()
        .map_err(device)?;
    stream.request_start().map_err(device)?;
    Ok(stream)
}
