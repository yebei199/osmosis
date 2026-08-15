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
    /// 这一帧点云的封面该怎么办。平帧恒为 [`CoverUpdate::Unchanged`] ——
    /// 一张封面是兆级的字节,每帧搬一次纯属白耗。
    pub cover: CoverUpdate,
    /// 视觉区里的指针,驱动涟漪与拖动旋转。
    pub pointer: VizPointer,
    /// 当前视觉预设的编号,见 `.slint` 的 `viz-preset`。越界由消费方兜底。
    pub preset: i32,
    /// 这一帧要不要遮挡层。
    ///
    /// 遮挡层是逐像素深度合成的那一半:有一张**深度卡片**要被粒子挡住时才需要。
    /// 现在那张卡片是世界空间标注卡,所以这里跟着它在不在画面里走 —— 锚点转出
    /// 画面的那些帧,第二遍全场景绘制立刻停。歌词不算,它画在粒子之上
    /// (见 `docs/adr/0010` 的「歌词是例外」)。
    ///
    /// 为假时 render3d 把那台相机整个关掉。
    pub needs_occluder: bool,
}

/// 点云封面这一帧的去向。
///
/// 三态而不是 `Option`:换歌与拿到新封面**是两件事**,中间隔着几百毫秒的网络。
/// 两者挤进一个 `Option` 的话,「还没有新的」与「这一首没有」长得一样,
/// 于是点云会一直挂着上一首的封面(见 `docs/adr/0013` 之后的那个 bug、
/// 以及 `CONTEXT.md`「封面点云」)。
#[derive(Default)]
pub enum CoverUpdate {
    /// 没有新消息,保持现状。绝大多数帧都是这个。
    #[default]
    Unchanged,
    /// 换歌了,先退回渐变 —— 旧封面配新歌比空着更误导。
    Clear,
    /// 新封面到了,换上并起一次切歌过渡。
    ///
    /// `Arc` 是因为这张图有两个去处:点云,以及系统媒体控件(安卓要拿它转
    /// `Bitmap`)。兆级的字节拷两份没有意义 —— 两边都只读。
    Show(std::sync::Arc<VizCover>),
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

/// 解出来的封面裸像素,与 [`VizCover`] 是同一个东西 —— 解码的出口就是 seam 的入口,
/// 中间那一次逐字段拷贝没有意义。
///
/// 名字留两个,是因为两端说的是两件事:`cover` 模块产的是「这张封面的像素」,
/// seam 传的是「这一帧要送进点云的封面」。类型别名而非新结构体,则是因为
/// **定义必须在 wasm 上也存在** —— `mod cover` 只在原生编译(它要 `image`),
/// 而 `CoverFeed` 是两端共用的。
pub type CoverPixels = VizCover;

/// 一帧播放页视觉:三张图,外加标注卡这一帧的挂点。
///
/// 覆层按「warp → 粒子 → 标注卡 → 遮挡层(裁到卡片)→ 控制簇」五层合成
/// (docs/adr/0010)。无 bevy 的端把场景与遮挡给成空图(width 0),
/// 覆层自动退回第一步的 warp 形态,.slint 里零平台判断。
pub struct VizImages {
    /// 反馈 warp 背景,铺满整窗。
    pub warp: slint::Image,
    /// 粒子场景,透明底,叠在 warp 之上。
    pub scene: slint::Image,
    /// 遮挡层:只含比标注卡锚点更近的片元,由 .slint 裁到卡片矩形。
    pub occluder: slint::Image,
    /// 标注卡这一帧挂在视口的哪一点,归一到 0..1、左上原点。
    ///
    /// `None` 表示锚点转出了画面或跑到相机背后,卡片这一帧不显示。无 bevy 的端
    /// 恒为 `None` —— 没有场景也就没有可锚定的物体,`.slint` 里同样零平台判断。
    pub anchor: Option<(f32, f32)>,
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
