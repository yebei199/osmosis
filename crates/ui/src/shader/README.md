# shader

喂给 GPU 装饰层的 seam 数据与它们的数学(docs/design/handoff-shaders.md)。

画是 render3d 画的,这里只产出「这一帧给它什么」:极光的三团光斑由封面取色
决定(`aurora`)、光带按钮的振幅朝目标收敛(`aurora_btn`)、导航水滴的位置与
转场判定(`nav_glass`)。

一个语义色都不在这里产出 —— 颜色的唯一来源是 `slint/theme.slint`。
seam 类型是 POD,apps/* 在接缝处平凡拷成 render3d 那侧的镜像类型,两边互不依赖。
