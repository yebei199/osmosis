//! 小米侧:NDK AAudio 出声与录音,adb shell 下直接跑的原生可执行文件,不打 APK。
//!
//! 呈现时刻取自 `AAudioStream_getTimestamp(CLOCK_MONOTONIC)`:它给出「第 P 帧在 T 时刻出声」,
//! 这一块第一帧(本端累计写到第 W 帧)的出声时刻就是 T + (W − P) / 采样率。
//! 流刚起来那几块拿不到时间戳,退回「现在 + 缓冲区长度」的估算,并如实计数。

use std::ffi::c_void;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ndk::audio::{
    AudioCallbackResult, AudioDirection, AudioError, AudioFormat, AudioInputPreset, AudioStream,
    AudioStreamBuilder, Clockid,
};

use crate::clock::mono_ns;
use crate::player::{Core, Shared};

const CHANNELS: i32 = 2;

pub struct Output {
    stream: AudioStream,
    pub rate: i32,
    pub disconnected: Arc<AtomicBool>,
}

impl Output {
    pub fn xruns(&self) -> i32 {
        self.stream.x_run_count()
    }

    pub fn device_id(&self) -> i32 {
        self.stream.device_id()
    }
}

/// 开一条输出流并开始放。路由切换(插拔耳机、连上蓝牙)会让流断开,
/// `disconnected` 被置位,由调用方关掉重开 —— 错误回调里不许重开流。
pub fn start(
    media: Arc<Vec<f32>>,
    media_rate: u32,
    shared: Arc<Shared>,
    gain: f32,
) -> Result<Output, String> {
    let disconnected = Arc::new(AtomicBool::new(false));
    let flag = disconnected.clone();
    let requested_rate = media_rate as i32;
    let mut core: Option<Core> = None;
    let mut written: i64 = 0;
    let stream = AudioStreamBuilder::new()
        .map_err(|e| e.to_string())?
        .direction(AudioDirection::Output)
        .format(AudioFormat::PCM_Float)
        .channel_count(CHANNELS)
        .sample_rate(requested_rate)
        .data_callback(Box::new(move |stream: &AudioStream, data: *mut c_void, frames: i32| {
            let rate = stream.sample_rate();
            let core = core.get_or_insert_with(|| {
                Core::new(media.clone(), f64::from(media_rate), f64::from(rate), shared.clone(), gain)
            });
            let now = mono_ns();
            let (present, measured) = match stream.timestamp(Clockid::Monotonic) {
                Ok(ts) => (
                    ts.time_nanoseconds + (written - ts.frame_position) * 1_000_000_000 / i64::from(rate),
                    true,
                ),
                Err(_) => (
                    now + i64::from(stream.buffer_size_in_frames()) * 1_000_000_000 / i64::from(rate),
                    false,
                ),
            };
            // SAFETY: AAudio 保证 data 指向 frames × 声道数 个 f32。
            let out = unsafe {
                std::slice::from_raw_parts_mut(data.cast::<f32>(), (frames * CHANNELS) as usize)
            };
            core.fill(out, CHANNELS as usize, present, now, measured);
            written += i64::from(frames);
            AudioCallbackResult::Continue
        }))
        .error_callback(Box::new(move |_stream, error| {
            eprintln!("AAudio 输出流出错: {error:?}");
            flag.store(true, Ordering::Relaxed);
        }))
        .open_stream()
        .map_err(|e| e.to_string())?;
    stream.request_start().map_err(|e| e.to_string())?;
    let rate = stream.sample_rate();
    Ok(Output {
        stream,
        rate,
        disconnected,
    })
}

/// 录 `secs` 秒麦克风到 16 位单声道 WAV。阻塞读,文件写在读线程上 —— 这里没有回调,可以。
pub fn record(path: &str, secs: u32) -> Result<(), String> {
    let stream = AudioStreamBuilder::new()
        .map_err(|e| e.to_string())?
        .direction(AudioDirection::Input)
        .format(AudioFormat::PCM_I16)
        .channel_count(1)
        .sample_rate(48_000)
        // 不要 AGC、降噪、回声消除:它们会搬动脉冲的时刻与形状。
        .input_preset(AudioInputPreset::Unprocessed)
        .open_stream()
        .map_err(|e| e.to_string())?;
    let rate = stream.sample_rate();
    eprintln!(
        "录音: {rate}Hz, 设备 {}, 预设 {:?}",
        stream.device_id(),
        stream.input_preset()
    );
    stream.request_start().map_err(|e| e.to_string())?;
    let total = (secs as usize) * rate as usize;
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?);
    file.write_all(&crate::wav::header(1, rate as u32, total as u32))
        .map_err(|e| e.to_string())?;
    let started = mono_ns();
    let mut buf = vec![0i16; 4_800];
    let mut got = 0usize;
    while got < total {
        let want = (total - got).min(buf.len());
        // SAFETY: buf 有 want 个 i16 的空间,单声道。
        let n = match unsafe { stream.read(buf.as_mut_ptr().cast(), want as i32, 1_000_000_000) } {
            Ok(n) => n as usize,
            // ndk 0.9 把正数(读到的帧数)也塞进 from_result,于是它变成了 `__Unknown(帧数)`。
            Err(AudioError::__Unknown(n)) if n > 0 => n as usize,
            Err(e) => return Err(e.to_string()),
        };
        for s in &buf[..n] {
            file.write_all(&s.to_le_bytes()).map_err(|e| e.to_string())?;
        }
        got += n;
    }
    file.flush().map_err(|e| e.to_string())?;
    eprintln!(
        "录音结束: {got} 帧,历时 {:.1}s,xrun {}",
        (mono_ns() - started) as f64 / 1e9,
        stream.x_run_count()
    );
    let _ = stream.request_stop();
    std::thread::sleep(Duration::from_millis(50));
    Ok(())
}
