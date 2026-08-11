# Osmosis

Osmosis 要把「[Slint](https://slint.dev) UI + Bevy 3D + 同一个 wgpu device」这条
多端融合架构立住:一个进程、一块显存、一条类型系统,UI 与 3D 之间什么都不隔,
一份代码出 desktop / android / web。音乐应用是它的首个载体,搜歌、边下边播、
多设备同播、封面点云,每一项都在真实产品的压力下检验这套架构;这条更贵的路
买到了什么、还能长成什么,见 [`docs/note/vision.md`](docs/note/vision.md)。
载体自己的路线(应用内 AI 助手、从听过看过的内容攒生词表、资讯与视频源)
继续往前走,但项目存在的理由是架构本身。

**端**:

| 端              | 状态                                |
|-----------------|-------------------------------------|
| linux           | 能构建、能运行                      |
| android         | 能构建、能运行(真机)                |
| web / iOS       | 能编译(CI 保证),尚未打包            |
| windows / macOS | 复用 `apps/desktop`,只差一个 target |

**功能**(与端正交 —— 安卓能跑不等于安卓上什么都有):

| 功能                    | 状态                                               |
|-------------------------|----------------------------------------------------|
| 音乐播放                | 能用。搜歌、边下边播、上下首、随机                 |
| 同播                    | 能用。多台设备点对点同放一首                       |
| 3D 可视化               | 能用(desktop / android)。播放页的封面点云          |
| AI agent                | 未开工。形态已定:应用内助手,把应用已有能力当工具调 |
| 背单词                  | 未开工。词从应用内的内容里抽,不做独立词库          |
| 内容源接入(资讯 / 视频) | 未开工                                             |

后三行是载体路线的意图,一行代码都还没有;任务总账在 [`docs/TODO.md`](docs/TODO.md)。

界面有两种**版式**,由运行时窗口宽度切换(< 600px 导航在底部,否则在左侧竖栏),桌面上把窗口
拖窄就能看到移动端的样子,见 [`docs/adr/0007`](docs/adr/0007-layout-mode-by-width-not-by-platform.md)。

**提交前跑 `just ci`** —— 它逐字复述 `.github/workflows/ci.yml` 的命令序列。
`dev` 分支上的 push 不触发 CI,这是唯一的防线。

## 文档去哪找

| 想知道                         | 看                                                       |
|--------------------------------|----------------------------------------------------------|
| 术语的含义与边界               | [`CONTEXT.md`](CONTEXT.md)                               |
| 某个选择为什么是这样           | [`docs/adr/`](docs/adr/)                                 |
| 这套选型的能力上限             | [`docs/note/vision.md`](docs/note/vision.md)             |
| UI 的设计原则与硬规则          | [`docs/design.md`](docs/design.md)                       |
| 接下来要做什么                 | [`docs/TODO.md`](docs/TODO.md)                           |
| 怎么装机、调试、看界面、打 APK | [`docs/note/dev-workflow.md`](docs/note/dev-workflow.md) |
| AI 助手在本仓库怎么干活        | [`AGENTS.md`](AGENTS.md)                                 |

## 目录结构

Cargo workspace。依赖方向严格单向,反向永久禁止:

```
apps/*        ─→  ui  ─┬─→  app-core  ─┐
                       ├─→  api  ──────┼─→  contract
                       └─→  syncplay  ─┘
                                └─→  audio  ←─  ui

apps/desktop  ─→  render3d                     不经过 ui
apps/android  ─┘
```

```
crates/contract/   线上格式(响应体、协议版本号)。依赖只允许 serde
crates/app-core/   客户端领域:状态与改变状态的规则。不依赖 slint,不做 IO
crates/api/        HTTP 客户端。native/wasm 的差异在此吸收,不向上传播
crates/audio/      把一条直链变成声音,并吸收各端音频后端的差异
crates/syncplay/   信令客户端与设备之间的 WebRTC 连接
crates/render3d/   bevy 在共享 wgpu device 上离屏渲染,产出 slint::Image
crates/ui/         Slint 界面声明 + 组装点:把 api 注入 app-core
apps/desktop/      桌面平台入口(linux / windows / macOS)
apps/android/      Android 平台入口(cdylib)+ gradle/ 打包工程
apps/ios/          iOS 平台入口(staticlib)。只验证编译,打包需 macOS
apps/web/          Web 平台入口(cdylib + wasm-bindgen)。只验证编译
server/            axum 后端:共享 contract,并把 bang-dream 的 gRPC 翻成 HTTP/JSON
server/proto/      那份 gRPC 契约的副本。上游在 bang-dream,它是独立仓库
assets/            图标源(一份 svg,三端都从它派生)与桌面的 .desktop
docker/            Docker 构建工作流(给没有 nix 的机器);见 docker/README.md
Android.nix        NixOS 本机原生工具链(nix-shell)
xtask/             构建逻辑(`cargo xtask android`),容器/本机通用
```

`app-core` 不知道 `api` 的存在:网络由 `ui` 注入。这既让领域逻辑能脱离网络单测,
也让 `Send` 约束被关在 `api` 内部,`app-core` 因此能原样编到 wasm。见
[`docs/adr/0002`](docs/adr/0002-send-boundary-lives-inside-api-crate.md)。

`audio` 与 `syncplay` 只在非 wasm 下挂进 `ui`:cpal 在 wasm 上不存在,无条件依赖会让
web 端编不过。`render3d` 则绕过 `ui` 直接挂在平台入口上 —— 它是 desktop 与 android
各自把 bevy 装进自己渲染循环的事,ui 只收一张 `slint::Image`。

裸 `cargo build` 只构建桌面链路(workspace 的 `default-members`)。
android / web / ios / server 一律靠 `-p` 显式构建 —— 因为 `android_main` 在宿主机
target 上根本编不过。

## 跑起来

只想看界面,一条命令,不需要后端:

```sh
just desktop-dev
```

想让它出声,四个终端。前置条件:另外 clone 一份
[bang-dream](https://github.com/yebei199/bang-dream)(把网易云等平台的加密与异构响应
收敛成统一 gRPC 接口的聚合层),用 `BANG_DREAM_REPO` 指向它(默认找同级目录的
`../bang-dream`),以及一次扫码登录。

```sh
just pg                      # 终端 0:Postgres(账号、本地歌单、播放事件)
just bang-dream-login        # 首次:扫码登录网易云,凭据按账号各存一份
just bang-dream              # 终端 1:gRPC,监听 127.0.0.1:50051
INVITE_CODE=... just server-dev   # 终端 2:axum,监听 127.0.0.1:3000
just desktop-dev             # 终端 3:「Music」页搜歌、点一首出声
```

到 bang-dream 的连接是惰性的 —— 它没起来时后端照常启动,请求到来才失败并映射成 502,
两个进程因此没有启动顺序约束。**数据库不同**:连不上就不启动,因为没有它连登录都办不成,
带着一个必然 500 的服务活着只会更难查。

音乐相关的路由都要登录态(`Authorization: Bearer`),账号由 `/register` 与 `/login`
取得,注册需要 `INVITE_CODE` —— 服务面向公网,没有这道门任何人都能开户
(见 [`docs/adr/0017`](docs/adr/0017-accounts-with-per-user-platform-credentials.md))。
账号同时是网易云凭据的分片键:每个账号绑自己的网易云登录。

对客户端而言 gRPC 不存在:它只见到 `/search`、`/play/{id}` 这样的 HTTP/JSON,
形状由 `contract` crate 定义。歌单也一样归一:`/playlists` 给的是一张列表,
平台歌单与本地歌单在里面靠 `source` 区分,客户端不必知道它们来自两个地方
(见 [`docs/adr/0016`](docs/adr/0016-playlist-split-by-data-ownership.md))。gRPC 的价值在 axum↔bang-dream 那一段 ——
Go 与 Rust 两侧从同一份 `.proto` 生成,改了一边忘了另一边,构建直接失败。

客户端拿到直链后**边下边播**(`crates/audio`,rodio + stream-download),不整曲下载 ——
同播的主控要边解码边推给听众,等整首下完再开始推是不能接受的
(见 [`docs/adr/0008`](docs/adr/0008-syncplay-is-webrtc-media-p2p.md))。

同播的音频走 WebRTC 的媒体轨,设备之间**点对点**,axum 只做信令中转,不碰音频。
角色是行为决定的 —— 界面上点哪台设备,自己就成为主控,对方成为听众,没有「设为主控」
的开关。设备身份是主机名加进程号,所以同一台机器上开两个实例天然就是两台设备。

装机、调试、打 APK、让 AI 助手看见运行中的界面,都在
[`docs/note/dev-workflow.md`](docs/note/dev-workflow.md)。

## License

GPL-3.0,外加一条附加限制:OpenAI 及其关联公司不获得本许可证的任何授权
(含用作模型训练数据或推理输入)。详见 [LICENSE](LICENSE)。
