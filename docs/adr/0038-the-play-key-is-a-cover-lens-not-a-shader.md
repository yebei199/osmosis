# 播放键是封面透镜,光带按钮与 warp 退役

日期:2026-11-11。状态:已采纳。修订 [`0029`](0029-player-bar-is-main-bar-plus-drawer.md)
的「环形播放键的彩虹圈改为常驻」、[`0035`](0035-render-on-demand-with-one-wake-path.md)
的光环与光带两条、[`0010`](0010-play-page-decoration-in-3d-function-in-2d.md) 五层合成的底层。

## 背景

2026-11 的整体改版(设计稿与 token 见 `docs/design.md`)把「玻璃」收成唯一的签名材质,
只许出现在三处:播放键、正在播放的缩略图、导航指示器。其余一律不透明。

旧的播放键是一圈 warp 彩虹:render3d 里一条 192×192 的反馈式 fragment pass(`warp.rs`),
外加光带按钮那条合批 pass(`aurorabtn.rs`,fluid/glass/progress 等五个变体)为条身、
播放页次要键、空状态的「换一批推荐」各渲一张底。两条 pass 带来的是:

- 一个常驻的环境动效,放着歌就要帧(`0035` 为它加了 `needs_warp` 与「暂停时光环静止」);
- 第二个色族:彩虹圈是全光谱,与「绿是唯一的强调色」长期互相拉扯;
- 封面取色、尺寸回写槽(`bar-w`/`viz-bar-w`)、设置开关「动态极光按钮」这一整串接缝。

## 决策

- **播放键换成封面透镜**(`controls.slint` 的 `PlayLens`):圆形裁剪里画当前封面放大到
  190%、上移 8%,压一层 30% 暗幕,外圈一道 conic 进度环。全是 Slint 原生图元,没有 shader;
  进度环靠属性变化重画,不要帧。
- **光带按钮、warp 与封面取色整条删除**:render3d 的 `aurorabtn.*`、`warp*`,ui 的
  `shader/aurora*`,seam 上的 `AuroraBtnControls`、`VizControls::needs_warp`、`VizImages::warp`,
  `run_with_renderers` 的 `btn_frame` 参数,`Shell` 上的光带纹理、尺寸槽与 `aurora-cover-*`。
- **播放页底层改为糊封面**:每首歌解码时顺手出一张 48px 的糊图(`imagery/cover.rs`),
  Slint 拉伸铺满。无 GPU 的端与有 GPU 的端是同一张图,点云叠在它上面。
- **设置「动态极光按钮」改为「动态效果」**:关掉就是减弱动效 —— `.slint` 的弹簧换成 200ms
  ease-out(`Motion.calm`),日夜开关的弹簧也收成临界阻尼。存储键从 `aurora_buttons` 改为
  `motion`,旧值照搬(关过极光的人多半也不要回弹)。
- 3D 留下的只有三样:卡墙、播放页点云、导航玻璃(`navglass`)。

## 备选方案

**保留 warp,只把它的颜色收进绿板。** 不选:问题不只是颜色,是「放着歌就要帧」这件事本身。
透镜用封面做内容,本来就随歌变,不需要一条跟着音乐翻涌的 pass 来证明「在放」。

**透镜做真折射**(把 unicat-switch 那种 `asin(r)` 放大写成 shader)。不选:改版约定不加新
shader;均匀放大 190% 在 52px 的键上与真折射肉眼分不出。

## 后果

- 播放页之外,放着歌也一帧不要:`FrameLoop::viz_frame` 只剩播放页场景那一道门,
  `0035` 的规则 3「暂停时光环静止」随光环一起失效。
- seam 少了一个闭包,`apps/desktop` 与 `apps/android` 的入口各少一条 pass 的构建。
- web / iOS(已冻结)曾靠「warp 为空图」退回纯色,现在退回同一张糊封面,跨端差异少了一条。
- 将来要在条上加会动的东西,先过 `0035` 的两个问题,并且不许再起一条按钮级的 pass。
