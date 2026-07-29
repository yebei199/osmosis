# render3d

3D 桥:用 **bevy**(0.19 稳定版)在**共享的** wgpu-29 device 上离屏渲染,
产出两张 `slint::Image`(场景 + 遮挡层)交给 UI 层合成。

## 为什么存在 / 边界

主体是 Slint 应用,本 crate 只负责「Slint 做不来的 3D 效果 / 小游戏」那一块。

- **桌面 / android 入口硬依赖它**,web / ios 永不碰 —— 由 `xtask boundaries` 守住。
  曾经它是个 `bevy-3d` feature,那时有个 3D 演示页可关;演示页删掉之后播放页的粒子、
  warp 与导航选中器全长在这里,关掉就只剩静态版式,开关遂取消(见 `docs/adr/0011`)。
- `ui` 对 bevy / wgpu 一无所知:本 crate 把渲染结果作为 `slint::Image`
  经 `ui::run_with_renderers` 的 seam 交过去(见 `crates/ui`)。
- 要加新 shader 前先看
  [`docs/note/animated-background-and-compute.md`](../../docs/note/animated-background-and-compute.md):
  fragment 与 compute 的分界(判据是 scatter),以及持续动画与本 crate 两道省电门的冲突。

## 关键架构约束(见计划 `bevy-serialized-dove`)

1. **共享 device**:instance/adapter/device/queue 由本 crate 自建(Manual),
   同一套既注入 Slint 的 `require_wgpu_29`,也注入 bevy 的 `RenderCreation::manual`。
   Slint 只能采样属于自己 device 的纹理,所以共享是硬性要求。
2. **事件循环归 Slint**:bevy 禁用 `bevy_winit`、无头运行,由 Slint 的 `Timer`
   每帧驱动 `app.update()`,绝不调 `App::run()`。
3. **wgpu 版本对齐**:bevy 与 slint 必须共享同一 wgpu 大版本(现为 29,Cargo.lock
   里实为单份 `wgpu 29.0.4`)。升级任一方前先核对,否则 `wgpu::Texture` 类型不兼容。

## 导航侧栏液态玻璃选中器(`navglass.rs` + `navglass.wgsl`)

宽版式左侧导航栏的背景由一个**不经 bevy** 的独立全屏 fragment pass 画:暗底 + 一层微极光,
外加一块会在 tab 之间「流动」的圆角矩形 metaball —— 头(快)尾(慢)两个位置都朝当前选中槽
中心移动,行走时 smooth-union 拉出胶着的颈,静止时重合成单块,颈那一档折射自绘的极光背景。

为什么不长在 bevy 里:选中器是纯 2D 玻璃,没有 3D 场景/ECS。而 texture→`slint::Image` 的桥
(`Image::try_from(wgpu::Texture)`)本就与 bevy 无关,故在 `Scene` 暴露的**同一块共享 device**
上自起一个 pass 即可。思路见 `docs/note/slint-bevy-architecture-and-direction.md` 第八节。

- 几何(槽位、栏尺寸、lead/lag 动画位置)由 `app.slint` 的 `nav-*` 属性给出,**是唯一真相**;
  由 apps/* 在 seam 处翻译成 `NavParams`(POD,镜像 `ui::NavGlassControls`)。
- **只在切 tab 的转场期间重渲**(省电门在 `ui::nav_glass::nav_transition_active`),静止时 Slint
  复用上一帧纹理 —— 与播放页视觉的门相互独立,同守仓库不主动重绘的省电取向。
- 分工:玻璃视觉在 shader;图标、标签、hover/点击仍由 Slint 画在上面。非 GPU 构建 `nav-bg`
  为空,侧栏退回 Slint 平底 + `NavItem` 自带高亮(渐进增强)。

## 播放页反馈 warp 视觉(`warp.rs` + `warp.wgsl`)

播放页(见 `CONTEXT.md`「播放页」与 `docs/adr/0010`)的全屏视觉,同样是**不经 bevy** 的
独立 fragment pass,但比 navglass 多两样:**两张目标纹理 ping-pong**(反馈机制每帧要采样
上一帧:朝中心缩、随低频转、按 decay 压暗,再叠新内容 —— 拖影与隧道感全来自这一步),
以及一张 512×2 的**音频纹理**(照 Shadertoy 约定:第一行频谱、第二行波形,由
`audio::spectrum` 在 CPU 上算好、apps 在 seam 处每帧送来,shader 侧采样代码与
Shadertoy 素材互通,见 `docs/note/visualization-surface-and-audio.md`)。

- 新内容是两圈极坐标可视化:外圈频谱环、内圈波形环,余弦调色板取紫/蓝/青一段与
  应用 aurora 同调;反馈能量用软限幅压住,不然高亮区几帧就烧成纯白。
- 省电门在 ui 侧(展开 ∧ 播放 ∧ 可见):门关着没人调 `render_frame`,Slint 复用
  上一帧纹理,GPU 归零;`time` 由 ui 的播放页时钟给,门关时钟停,重开从定格处继续。
- 两张目标纹理各自只导入 Slint 一次,每帧只翻转「画哪张、采哪张」。

## 播放页封面点云(`cloud.rs` + `cloud.wgsl` + `Scene`)

播放页的可视化主体(issue #22):**当前曲目的封面被采样成 183×183 ≈ 3.35 万颗粒子**,
每颗取封面上对应像素的颜色,整片随声音起伏。行为照抄参照物 Mineradio 的
`02-visual/00-pointer-cover-particles.js` 的 **preset 0(SILK,它的默认档)**。
`render_viz_frame` 是本 crate 里 bevy 侧唯一的驱动入口;主相机清屏透明,场景图叠在
warp 背景之上。

关键结构决策在 [`docs/adr/0012`](../../docs/adr/0012-cover-point-cloud-is-one-mesh-in-the-bevy-scene.md):
点云是 bevy 场景里的**一个 mesh**,不是三万多个实体,也不是又一个独立 wgpu pass。

- `cloud.rs` 是纯计算加胶水:`band_levels` 把频谱行拆低/中/高三段;`cloud_vertices`
  烘出那份建一次就不动的顶点缓冲(每颗粒子四个顶点拼一个方片,带格点 uv、角偏移、
  逐粒子随机数);`CloudMaterial` 是自定义材质,顶点布局把角偏移借在法线属性位上。
  单元测试盯的是网格本身(四角齐全、uv 不贴边、索引不越界),位移与观感走真实像素验收。
- `cloud.wgsl` 里是全部运动学:中频驱动的两层 simplex 噪声起伏、高频抖动、低频呼吸,
  全部只推 z —— 正面看是一张随音乐抖动的封面,侧面才看得出它有厚度。方片在**视图
  空间**摊开,粒子因此永远是正圆而不是随相机变斜的菱形。
- CPU 每帧只改材质的一块 uniform(时间 + 三段电平),几何一动不动。
- 透明度分平台:桌面 `AlphaMode::Blend` 画软边发光圆点,安卓 `AlphaMode::Mask` 画硬边。
  小米13(Adreno)上半透明小元素整片不显示是本仓踩过的坑。
- 材质关掉 prepass 与阴影:两者都会拿**默认**顶点着色器再跑一遍这份 mesh,而我们的
  顶点布局只有 `cloud.wgsl` 读得懂。着色器随二进制内嵌(`embedded_asset!`)——
  应用无头运行,没有 `assets/` 目录。

早先这里是一层与封面无关的**浮空尘埃**(1300 颗五色小球绕卡片飘),照抄的是参照物
同一目录下的 `01-float-skull-backcover.js` —— 那只是它封面粒子系统背后的配菜。
主体做出来之后配菜删掉,`band_levels` 是唯一留下的部分。

## 深度正确的 UI(遮挡层)

Slint 的合成没有深度:3D 画面对它只是一张 `Image`,任何 Slint 控件都恒在其上。要让场景
里的物体挡住一张 Slint 卡片,把合成拆成三层 —— **场景 → 卡片 → 遮挡层**。遮挡层由
**第二台相机**画:与主相机同位、同投影、同色调映射,只差两处 —— 清除色透明,且深度缓冲
不清到远平面而是清到卡片所在的深度。于是它只剩「比卡片更近」的片元,其余透明,Slint 那边
只做寻常的 alpha 合成。UI 不必先渲进纹理,Slint 也不必懂深度。

- **门槛值**是锚点(粒子场中心,即封面卡所在的深度)的 NDC 深度,每帧由 `Camera::world_to_ndc` 算出。bevy 用
  反向 Z(1 是近平面、0 是远平面),深度测试是 `GreaterEqual`,「清到锚点深度」正好筛掉
  更远的片元。锚点跑出视锥时退回近平面 → 遮挡层为空、卡片完整可见:宁可少一个效果,也
  不能把整幅场景糊在卡片上。
- **逐片元,不是逐物体**:横跨锚点平面的物体会被平面切开。CPU 侧按物体排序做不到,而
  「UI 整层贴在 canvas 上」的方案在原理上就做不到 —— 这正是这条路线买到的东西。
- **代价是几何提交两遍**。粒子用 12 段的圆片而非球体,正是为了让这一遍付得起。要省的话:
  卡片隐藏时关掉这台相机的 `is_active`,或改成采样深度纹理的一个全屏 pass(那需要自建
  渲染图节点与 WGSL,现在不值)。
- 遮挡层那张 Image 在 `app.slint` 里是**整窗**大小的(必须和场景那张逐像素对齐),
  用负偏移推回窗口原点,再由封面卡那层 `clip` 裁到卡片范围。不裁的话,近处的粒子会连
  控制簇一起盖住 —— 控制簇是界面外壳,不在场景里。

判断它有没有生效见 [`AGENTS.md`](../../AGENTS.md):**量卡片边框的像素,别看整幅图的观感**。

## 依赖版本

bevy 用 0.19 稳定版(crates.io)。BSN(`bsn!` 宏 + 场景系统)已随 0.19 发布,本 crate
采用之;它藏在 `bevy_scene` feature 后(见根 `Cargo.toml`),只拖纯 Rust 序列化 crate、
无系统库。

## 运行

需要 vulkan 运行期库,仓库根的 `slint.nix` 已经带上:

```
nix-shell slint.nix --run "cargo run -p app-desktop"
```

## 相机不随视口长宽比后撤

透视投影固定的是**垂直**视野,水平视野 = 垂直视野 × 长宽比,所以视口一竖(手机竖屏、
桌面拖窄的紧凑版式,长宽比可低到 0.26)横向排开的内容就会出画。这里**刻意不**为此后撤相机:
粒子场是铺满视野的环境效果,后撤只会把粒子缩成看不见的点 —— 小米13 竖屏 aspect 0.45 要后撤
2.2 倍,真机上实测粒子直接消失。相机恒在 `BASE_CAMERA_POS`,让粒子自然溢出画面上下。

早先为 3D 演示页写过一个 `pullback(aspect)`(参考长宽比 / 当前长宽比,封顶 4 倍,整体缩放
位置向量以保住俯视角),演示页删掉后它没有消费者了,一并删除。
