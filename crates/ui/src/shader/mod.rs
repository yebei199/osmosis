//! 喂给 GPU 装饰层的 seam 数据与它们的数学(docs/design/handoff-shaders.md)。
//!
//! 画是 render3d 画的,这里只产出「这一帧给它什么」:极光的三团光斑取自封面,
//! 光带按钮的振幅朝目标收敛,导航水滴的位置与转场判定。
//! 一个语义色都不在这里产出 —— 那些在 `slint/theme.slint`。

pub mod aurora;
pub mod aurora_btn;
pub mod nav_glass;
