//! 电脑侧出声:cpal。呈现时刻 = 回调时的单调钟 + (playback − callback),
//! 后者是 cpal 从 ALSA 的 `snd_pcm_status` delay 算出来的。

use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::clock::mono_ns;
use crate::player::{Core, Shared};

pub struct Output {
    _stream: cpal::Stream,
    pub device: String,
    pub rate: u32,
}

pub fn start(
    media: Arc<Vec<f32>>,
    media_rate: u32,
    shared: Arc<Shared>,
    gain: f32,
) -> Result<Output, String> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("没有默认输出设备")?;
    let name = device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "?".to_owned());
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let channels = usize::from(supported.channels());
    let rate = supported.sample_rate();
    let config: cpal::StreamConfig = supported.into();
    let mut core = Core::new(media, f64::from(media_rate), f64::from(rate), shared, gain);

    let stream = device
        .build_output_stream(
            &config,
            move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                let now = mono_ns();
                let ts = info.timestamp();
                let (delay, measured) = match ts.playback.duration_since(&ts.callback) {
                    Some(d) => (d.as_nanos() as i64, true),
                    None => (0, false),
                };
                core.fill(data, channels, now + delay, now, measured);
            },
            |e| eprintln!("cpal 输出出错: {e}"),
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Output {
        _stream: stream,
        device: name,
        rate,
    })
}
