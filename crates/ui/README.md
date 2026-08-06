# ui

UI 层:界面的声明,以及界面与客户端领域(`app-core`)之间的双向绑定。本 crate 同时是
**组装点**:把 `api` 的请求函数、`audio` 的播放器注入 `app-core`,几者互不相识;依赖
方向单向,反向永久禁止(`docs/adr/0003`,CONTEXT.md「UI 层」)。平台入口(apps/*)
初始化好渲染后端后把控制权交给这里的 `run` / `run_with_renderer(s)`。

## 文件与子目录

- `slint/app.slint`:整个界面的声明。两个 tab(卡片主页、音乐页)、宽/紧凑
  两种版式(由宽度决定,`docs/adr/0007`)、常驻于音乐页的控制条,以及从控制条展开的
  播放页覆层(CONTEXT.md「播放页」),覆层分层合成:warp 背景 → 粒子场景 →
  歌名与歌词 → 复用 ControlCluster 的功能层(`docs/adr/0010`)。**深度卡片
  目前一张也没有**:歌词曾经是一张,后来改成与歌名同层画在粒子之上,遮挡层
  因此不再逐帧渲染(`VizControls::needs_occluder` 为假时相机整个关掉),
  能力留着等下一张。无 GPU 端粒子为空图,自动退回 warp 形态。
- `slint/theme.slint`:色板。**界面上每一个颜色都从 `Theme` 取,不在别处写死** ——
  在此之前它们散在 9 个文件里,133 处、67 种,其中 48 种只出现一次。20 个语义 token,
  深浅两列(`Theme.dark` 切换);强调色的三种状态由 `accent` 派生,不各给一个名字。
  三族表面刻意分开:常驻界面、沉浸层(播放页压在封面与点云上的那块底)、错误横幅。
  三处**不**走色板:日月开关那幅画的颜色(天空/太阳/月亮是图形不是语义)、
  `glass.slint`(归 3D 那层)、帧率读数。见 `docs/change_log/2026-08-06/theme-palette.md`。
- `slint/glass.slint`:极光背景与玻璃卡片两个可复用组件,视觉基调的唯一来源。
- `slint/tracklist.slint` / `playlists.slint` / `artists.slint`:三列可滚的行。都是
  `ListView`(而非 `Flickable` + `for`)—— 歌单详情能到近千行,全量实例化配上每行
  一张封面就是 GB 级内存。代价是 ListView 没有 `spacing`,行间那 4px 由卡片上下
  各让 2px 让出来。
- `src/lib.rs`:crate 门面与帧驱动。`build_ui` 完成绑定;`run_with_renderers` 把渲染
  通知回调当帧泵,依次驱动导航选中器(转场门)、播放页 warp(展开∧播放∧可见门)
  与 3D 场景(`render-active` 门),三道省电门相互独立。
- `src/music.rs`:音乐页绑定。搜索/每日/红心、队列点播、控制条、自动续播、
  下一首的预取(判据 `should_prefetch`,认领判据 `take_prefetched`)、
  开机自检,以及播放页元数据(歌名/歌手/封面/歌词)的推送。显示格式化都在这层做。
  进度与音量也在这里接:音量每动一下就存回本地设置(`api::settings`),
  而**跳转是异步的** —— `bind_seek` 只把请求送到解码线程并立刻挂上「缓冲中」,
  落地与失败由每秒那趟轮询从 `audio::SeekState` 上取(`push_seek_state`)。
  `LyricFeed` 是歌词的取用口:行表随换歌整批替换并递增代际,播放页每帧问它
  当前行,靠 (代际, 行号) 判断该不该推新值 —— 每帧无脑推会标脏、破坏省电门。
- `src/media.rs`:系统媒体控件的接缝(CONTEXT.md「系统媒体控件」)。定义
  `NowPlaying` / `MediaCommand` / `MediaHooks` / `MediaControls`,后端由平台入口注入
  (`docs/adr/0020`)—— zbus 那份在 apps/desktop,JNI 那份在 apps/android。换算全在
  这一侧做完:`Play` 与 `Toggle` 的区分、相对跳转换成绝对位置、绝对位置换成 `seek`
  要的比例、`SetShuffle` 那个绝对值换成界面上唯一那个切换回调,后端因此不必记住
  任何状态。封面给两份(CDN 链接给 MPRIS,裸像素给安卓
  转 Bitmap),两份 ui 本来都攥着。`Bridge` 负责去重:推送搭 1Hz 的续播轮询,
  不去重的话一首歌要往外发两百多次内容相同的状态变更。`dispatch` 把外面按的键
  翻成 `.slint` 的回调 —— 只调回调、不碰状态,那一套规矩在 `music::bind_controls`
  里已经有了,重写一遍就会长歪。
- `src/viz.rs`:播放页可视化的 seam 数据(入参 `VizControls`,出参三图的
  `VizImages`,均 POD)与音频载荷的取帧 helper。wasm 侧数据源恒空,取帧代码
  无平台判断。
- `src/progress.rs`:进度的格式化与换算。`progress_text` / `ratio` 把位置和
  总长排成一行字与一个比例,`seek_target` 是 `ratio` 的逆 —— 总长不知道或比例
  非有限时给 `None` 而不是 0 秒:前者是"倒回开头",后者会让
  `Duration::from_secs_f64` 直接 panic。
- `src/account.rs`:登录页绑定,以及"这次失败该说哪句话"。会话失效由
  `handle_session_expiry` 统一处理,任何一条路由拿到 `unauthorized` 都走它。
- `src/cover.rs`:封面字节 → `slint::Image`。`decode` 出原分辨率(播放页那张大图
  要它),`decode_thumbnail` 出 96px 的小图(列表一屏要几十张,只能出小的)。
  封面 CDN 会过期给回 HTML 页,失败路径按常态处理,返回 `None` 不 panic。仅原生编译。
- `src/artwork.rs`:歌单封面表。内存 → 磁盘 → CDN 三级,键取歌单 id 而不是
  URL(CDN 会换 URL,按 URL 键整个缓存跟着作废)。回填按 id 重扫模型,
  不按下标 —— 图回来时列表可能已经换了一批。
- `src/thumbnail.rs`:曲目行的缩略图。与 `artwork` 是**两套**,差别在键:这边按
  封面 URL 存,因而一张专辑封面被十几首歌共用时只取一次、只占一份。行滑进可见区
  才取(信号来自 `tracklist.slint` 的 `changed wanted`,ListView 复用实例不重跑
  `init`),滚动停 150ms 才发请求且只发最后一批,内存 512 张 LRU。磁盘那一层按 URL
  的 blake3 存在 `covers/tracks/`,由 `api::sweep_track_artwork` 在启动时按 mtime
  削回上限 —— URL 会换,不削就只涨不减。仅原生编译。
- `src/liked.rs`:红心集合。服务端的曲目不带"喜不喜欢",取一次全量标识
  存成集合,推行时本地比对。
- `src/playlist.rs`:歌单的读与写绑定 —— 建、改名、删、批量加、移除。
  哪些歌单可写由 `is_editable` 判(平台歌单只读)。
- `src/search.rs`:搜索的三个页签(单曲/歌手/歌单)。关键词记在 Rust 侧,
  因为输入框长在一个 `if` 里,Rust 引用不到它。
- `src/syncplay.rs`:同播绑定(仅原生)。设备名册、推流/收听的 UI 状态。
- `src/nav_glass.rs`:导航选中器的 seam 数据与转场省电门判定。
- `src/fps.rs`(lib.rs 内模块):诚实即时帧率计,运行期 `OSMOSIS_FPS` 开关。
- `fonts/`:中文子集字体。硬编码中文必须落在子集里,`cargo test -p ui` 的
  glyph 测试守着;平台数据(歌名等)不指定字体、走系统字体。
- `tests/banner.rs`:断流横幅的界面行为,无头跑(`i-slint-backend-testing`
  的软件后端 + 模拟时钟,不要窗口也不要显卡)。补的是纯函数够不着的那一半 ——
  文案对不对是 `describe_stream_loss` 的事,看不看得见是 `.slint` 里那句
  `if root.banner-text != ""` 的事。
- `tests/thumbnail.rs`:曲目行封面槽位的界面行为,同样无头跑。其中
  `a_long_list_only_renders_what_fits` 守的是虚拟化本身 —— 退回全量渲染不会报错,
  只会让内存悄悄涨上去。
- `build.rs`:slint-build 编译 `.slint`。debug 档一律带元素调试信息 ——
  `ElementHandle` 与 Slint MCP 的元素树都以它为前提,而少了它两者不报错、
  只是查不到任何元素。release 不带。
