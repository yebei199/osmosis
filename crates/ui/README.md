# ui

UI 层:界面的声明,以及界面与客户端领域(`app-core`)之间的双向绑定。本 crate 同时是
**组装点**:把 `api` 的请求函数、`audio` 的播放器注入 `app-core`,几者互不相识;依赖
方向单向,反向永久禁止(`docs/adr/0003`,CONTEXT.md「UI 层」)。平台入口(apps/*)
初始化好渲染后端后把控制权交给这里的 `run` / `run_with_renderer(s)`。

## 文件与子目录

- `slint/app.slint`:整个界面的声明。两个 tab(卡片主页、音乐页)、宽/紧凑
  两种版式(由宽度决定,`docs/adr/0007`)、常驻于音乐页的控制条,以及从控制条展开的
  播放页覆层(CONTEXT.md「播放页」),覆层分层合成:warp 背景 → 粒子场景 →
  封面深度卡与歌词 → 遮挡层(同一张图分别裁到封面与歌词两个矩形,二者同处一个
  z 平面,粒子因此从封面与字的前后掠过)→ 复用 ControlCluster 的功能层
  (`docs/adr/0010`)。无 GPU 端粒子与遮挡为空图,自动退回 warp 形态。
- `slint/glass.slint`:极光背景与玻璃卡片两个可复用组件,视觉基调的唯一来源。
- `src/lib.rs`:crate 门面与帧驱动。`build_ui` 完成绑定;`run_with_renderers` 把渲染
  通知回调当帧泵,依次驱动导航选中器(转场门)、播放页 warp(展开∧播放∧可见门)
  与 3D 场景(`render-active` 门),三道省电门相互独立。
- `src/music.rs`:音乐页绑定。搜索/每日/红心、队列点播、控制条、自动续播、
  开机自检,以及播放页元数据(歌名/歌手/封面/歌词)的推送。显示格式化都在这层做。
  `LyricFeed` 是歌词的取用口:行表随换歌整批替换并递增代际,播放页每帧问它
  当前行,靠 (代际, 行号) 判断该不该推新值 —— 每帧无脑推会标脏、破坏省电门。
- `src/viz.rs`:播放页可视化的 seam 数据(入参 `VizControls`,出参三图的
  `VizImages`,均 POD)与音频载荷的取帧 helper。wasm 侧数据源恒空,取帧代码
  无平台判断。
- `src/cover.rs`:封面字节 → `slint::Image`。封面 CDN 会过期给回 HTML 页,
  失败路径按常态处理,返回 `None` 不 panic。仅原生编译。
- `src/syncplay.rs`:同播绑定(仅原生)。设备名册、推流/收听的 UI 状态。
- `src/nav_glass.rs`:导航选中器的 seam 数据与转场省电门判定。
- `src/scene_params.rs`:3D 热调参数的信任边界解析(clamp,坏值退回上一个好值)。
- `src/fps.rs`(lib.rs 内模块):诚实即时帧率计,运行期 `SLINT_STUDY_FPS` 开关。
- `fonts/`:中文子集字体。硬编码中文必须落在子集里,`cargo test -p ui` 的
  glyph 测试守着;平台数据(歌名等)不指定字体、走系统字体。
- `tests/banner.rs`:断流横幅的界面行为,无头跑(`i-slint-backend-testing`
  的软件后端 + 模拟时钟,不要窗口也不要显卡)。补的是纯函数够不着的那一半 ——
  文案对不对是 `describe_stream_loss` 的事,看不看得见是 `.slint` 里那句
  `if root.banner-text != ""` 的事。
- `build.rs`:slint-build 编译 `.slint`。debug 档一律带元素调试信息 ——
  `ElementHandle` 与 Slint MCP 的元素树都以它为前提,而少了它两者不报错、
  只是查不到任何元素。release 不带。
