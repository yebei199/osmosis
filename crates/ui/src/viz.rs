//! 播放页可视化的 seam 数据:UI 侧攒好的一帧控制量,POD,不涉 wgpu 类型。
//!
//! 与 [`crate::SceneControls`] / [`crate::NavGlassControls`] 同一个镜像分离模式:
//! apps/* 在 seam 处把它平凡拷给 render3d 的 `WarpPass`,ui 与 render3d 互不依赖。

/// 音频载荷字节数:512 频谱 + 512 波形(`audio::spectrum` 的布局,
/// 与 render3d 的 `AUDIO_BYTES` 手工对齐 —— 三个 crate 互不依赖)。
pub const VIZ_AUDIO_BYTES: usize = 1024;

/// 一帧 warp 视觉的控制量。
pub struct VizControls {
    /// 播放页时钟,秒。门关着时不走,重开门画面从定格处继续。
    pub time: f32,
    /// 频谱行在前、波形行在后,共 [`VIZ_AUDIO_BYTES`] 字节。
    pub audio: [u8; VIZ_AUDIO_BYTES],
}

/// 可视化的数据来源。原生是频谱分析器的句柄;wasm 没有原生音频栈,
/// 用 `Infallible` 占位 —— 恒为 `None`,取帧代码不必写平台判断。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) type Source = Option<audio::spectrum::Analyzer>;
#[cfg(target_arch = "wasm32")]
pub(crate) type Source = Option<core::convert::Infallible>;

/// 取一帧音频载荷。没有分析器(wasm / 无声卡)给 `None`,这一帧不渲。
pub(crate) fn payload(
    source: &Source,
) -> Option<[u8; VIZ_AUDIO_BYTES]> {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(analyzer) = source {
        let frame = analyzer.frame();
        let mut audio = [0u8; VIZ_AUDIO_BYTES];
        audio[..frame.spectrum.len()]
            .copy_from_slice(&frame.spectrum);
        audio[frame.spectrum.len()..]
            .copy_from_slice(&frame.waveform);
        return Some(audio);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = source;
    None
}
