//! 播放页可视化的 seam 数据:UI 侧攒好的一帧控制量,POD,不涉 wgpu 类型。
//!
//! 与 [`crate::SceneControls`] / [`crate::NavGlassControls`] 同一个镜像分离模式:
//! apps/* 在 seam 处把它平凡拷给 render3d 的 `WarpPass`,ui 与 render3d 互不依赖。

/// 音频载荷字节数:512 频谱 + 512 波形(`audio::spectrum` 的布局,
/// 与 render3d 的 `AUDIO_BYTES` 手工对齐 —— 三个 crate 互不依赖)。
pub const VIZ_AUDIO_BYTES: usize = 1024;

/// 一帧播放页视觉的控制量。
pub struct VizControls {
    /// 播放页时钟,秒。门关着时不走,重开门画面从定格处继续。
    pub time: f32,
    /// 频谱行在前、波形行在后,共 [`VIZ_AUDIO_BYTES`] 字节。
    pub audio: [u8; VIZ_AUDIO_BYTES],
    /// **换歌解出新封面的那一帧**才有值:点云要采的封面像素。
    ///
    /// 平帧恒为 `None` —— 一张封面是兆级的字节,每帧搬一次纯属白耗。
    /// 收到值的那一端据此换纹理并起一次切歌过渡。
    pub cover: Option<VizCover>,
    /// 视觉区里的指针,驱动涟漪与拖动旋转。
    pub pointer: VizPointer,
}

/// 视觉区指针的一帧状态,位置归一到 0..1(左上原点)。
///
/// `active` 为假表示指针不在视觉区里 —— 这一帧既不起涟漪也不拖动。
#[derive(Clone, Copy, Debug, Default)]
pub struct VizPointer {
    pub x: f32,
    pub y: f32,
    pub down: bool,
    pub active: bool,
}

/// 送给点云的封面像素:RGBA8,行优先,长度恒为 `width × height × 4`。
///
/// 与 [`crate::SceneControls`] 同一个镜像分离模式:POD,不涉 wgpu 与 bevy 类型,
/// apps/* 在 seam 处平凡拷给 render3d。
pub struct VizCover {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// 一帧播放页视觉的三张图:warp 背景、粒子场景、遮挡层。
///
/// 覆层按「warp → 粒子 → 封面卡 → 遮挡层(裁到卡片)→ 控制簇」五层合成
/// (docs/adr/0010)。无 bevy 的端把场景与遮挡给成空图(width 0),
/// 覆层自动退回第一步的 warp 形态,.slint 里零平台判断。
pub struct VizImages {
    /// 反馈 warp 背景,铺满整窗。
    pub warp: slint::Image,
    /// 粒子场景,透明底,叠在 warp 之上。
    pub scene: slint::Image,
    /// 遮挡层:只含比封面卡锚点更近的片元,由 .slint 裁到卡片矩形。
    pub occluder: slint::Image,
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
