//! 桌面输出：cpal。呈现时刻 = 回调那一刻的单调时钟 + cpal 给出的 `playback − callback`
//! (ALSA `snd_pcm_status` 的 delay;PipeWire 桌面上走的是 pipewire-alsa 插件)。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use ::cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::mixer::Mixer;
use rodio::{ChannelCount, SampleRate};

use super::{SharedMixer, fill, watch};
use crate::AudioError;
use crate::clock::monotonic_ns;
use crate::sync::SyncShared;

/// 开流、交出混音器，然后守着流直到被叫停。
pub(super) fn run(
    shared: Arc<SyncShared>,
    ready: &mpsc::Sender<Result<Mixer, AudioError>>,
    stop: &mpsc::Receiver<()>,
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
            let _ = ready.send(Err(AudioError::Device(error.to_string())));
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
    let (mixer, source) = rodio::mixer::mixer(channels, rate);
    let source: SharedMixer = Arc::new(Mutex::new(source));
    let broken = Arc::new(AtomicBool::new(false));

    let mut stream = match start(&device, &config.into(), &source, &shared, &broken) {
        Ok(stream) => stream,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(mixer));

    watch(stop, &|| broken.load(Ordering::Relaxed), || {
        log::warn!("输出流断了，重开默认设备");
        broken.store(false, Ordering::Relaxed);
        let reopened = default_device().and_then(|device| {
            let config = device
                .default_output_config()
                .map_err(|e| AudioError::Device(e.to_string()))?;
            start(&device, &config.into(), &source, &shared, &broken)
        });
        match reopened {
            Ok(fresh) => stream = fresh,
            Err(error) => {
                log::warn!("重开输出失败，稍后再试: {error}");
                broken.store(true, Ordering::Relaxed);
            }
        }
    });
    drop(stream);
}

fn default_device() -> Result<::cpal::Device, AudioError> {
    ::cpal::default_host()
        .default_output_device()
        .ok_or_else(|| AudioError::Device("没有默认输出设备".to_owned()))
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
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], info: &::cpal::OutputCallbackInfo| {
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
                broken.store(true, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| AudioError::Device(e.to_string()))?;
    stream
        .play()
        .map_err(|e| AudioError::Device(e.to_string()))?;
    Ok(stream)
}
