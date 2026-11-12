//! render3d 与 ui 之间的契约类型:指针、封面更新、一帧可视化的入参。

// BSN(next-gen 场景系统,bevy_scene feature)的 bsn! 宏、Scene/SceneList、
// World::spawn_scene 都已在 bevy::prelude 里,无需额外 use。见 rebuild_content。
// 0.19 起相机相关类型拆到 bevy_camera,facade 以 `bevy::camera` 再导出。
use slint::wgpu_29::wgpu;

/// bevy 与 slint 共享的 `wgpu::Texture` 类型别名(经 slint 的 wgpu_29 再导出,与 bevy 同一份 crate)。
pub type SharedTexture = wgpu::Texture;

/// 一帧里视觉区的指针状态,POD。镜像 `ui::VizPointer`,apps/* 在 seam 处平凡拷过来。
///
/// 位置归一到 0..1(左上原点)。`active` 为假表示指针不在视觉区里,这一帧不拖动。
#[derive(Clone, Copy, Debug, Default)]
pub struct Pointer {
    pub x: f32,
    pub y: f32,
    pub down: bool,
    pub active: bool,
}

/// 封面这一帧的去向。镜像 `ui::viz::CoverUpdate`。两个效果都收它,当前没在显示的那个也收 ——
/// 切过去时才不会挂着旧封面。
///
/// 三态而不是 `Option`:换歌与拿到新封面是两件事,中间隔着几百毫秒的网络,
/// 而封面常常根本拿不到。两者挤进一个 `Option`,画面就会一直挂着上一首
/// (见 `docs/adr/0014`)。
#[derive(Clone, Copy, Debug, Default)]
pub enum CoverUpdate<'a> {
    /// 没有新消息,保持现状。绝大多数帧都是这个。
    #[default]
    Unchanged,
    /// 换歌了,退回空白(点云是默认渐变,封面实物是一张没印字的空封套)。
    Clear,
    /// 新封面到了:(宽, 高, RGBA8)。
    Show(u32, u32, &'a [u8]),
}

/// 驱动一帧播放页视觉要的全部输入,POD。镜像 `ui::VizControls` 加上视口尺寸。
///
/// 打包而不是摊成一串参数:这几样每加一件,`render_viz_frame` 的签名就长一截,
/// 调用处也看不出哪个位置是谁。apps/* 在 seam 处把 ui 那份平凡拷成这一份。
#[derive(Clone, Copy, Debug, Default)]
pub struct VizFrame<'a> {
    /// 播放页时钟,秒。门关即冻结。
    pub time: f32,
    /// `spectrum` 布局的载荷,频谱行在前。只用前 512 字节拆频段。
    pub audio: &'a [u8],
    /// 这一帧封面该怎么办。平帧恒为 [`CoverUpdate::Unchanged`]。
    pub cover: CoverUpdate<'a>,
    /// 视觉区里的指针。
    pub pointer: Pointer,
    /// 播放页效果的编号:0 封面实物(默认)、1 点云。越界回默认(见 `effect::Effect`)。
    pub effect: i32,
    /// 紧凑版式(窗宽 < 600 逻辑像素)。主体在宽版式偏左、紧凑版式偏上,
    /// 由相机移轴做(见 `camera::lens`),场景图本身铺满整窗。
    pub compact: bool,
    /// 窗口的物理像素尺寸。与当前纹理不同就按需重建(动态分辨率),0 尺寸忽略。
    pub width: u32,
    pub height: u32,
}
