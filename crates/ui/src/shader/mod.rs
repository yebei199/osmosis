//! 喂给 GPU 装饰层的 seam 数据与它们的数学(docs/design/handoff-shaders.md)。
//!
//! 画是 render3d 画的,这里只产出「这一帧给它什么」:导航玻璃药丸的位置与转场判定。
//! 一个语义色都不在这里产出 —— 那些在 `slint/theme.slint`。

pub mod nav_glass;
