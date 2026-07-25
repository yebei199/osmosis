# Slint Study

一个横跨多端的 [Slint](https://slint.dev) 应用骨架 —— 目前是一个点击计数器,
外加一次真实的客户端-服务端往返。UI 用 Slint 编写,逻辑用 Rust 编写。

| 端 | 状态 |
|----|------|
| linux | 能构建、能运行 |
| android | 能构建、能运行(真机) |
| web / iOS | 能编译(CI 保证),尚未打包 |
| windows / macOS | 复用 `apps/desktop`,只差一个 target |

术语见 [`CONTEXT.md`](CONTEXT.md),架构决策见 [`docs/adr/`](docs/adr/),这套选型的能力
上限推演见 [`docs/note/vision.md`](docs/note/vision.md),AI 助手在本仓库干活前先读
[`AGENTS.md`](AGENTS.md)。

界面有两种**版式**,由运行时窗口宽度切换(< 600px 为紧凑版式,导航在底部;否则导航在左侧
竖栏)—— 桌面上把窗口拖窄就能看到移动端的样子,见
[`docs/adr/0007`](docs/adr/0007-layout-mode-by-width-not-by-platform.md)。截真实像素:

```sh
just shot        # 宽版式
just shot 420    # 紧凑版式;第二个参数指定开局页,如 `just shot 420 2` 直接进 3D 页
```

**提交前跑 `just ci`** —— 它逐字复述 `.github/workflows/ci.yml` 的命令序列。
`dev` 分支上的 push 不触发 CI,这是唯一的防线。

## 目录结构

Cargo workspace。依赖方向严格单向,反向永久禁止:

```
apps/*  →  ui  →  ┬─ app-core ─┐
                  └─ api ──────┴─→ contract
```

```
crates/contract/   线上格式(响应体、协议版本号)。依赖只允许 serde
crates/app-core/   客户端领域:状态与改变状态的规则。不依赖 slint,不做 IO
crates/api/        HTTP 客户端。native/wasm 的差异在此吸收,不向上传播
crates/ui/         Slint 界面声明 + 组装点:把 api 注入 app-core
apps/desktop/      桌面平台入口(linux / windows / macOS)
apps/android/      Android 平台入口(cdylib)+ gradle/ 打包工程
apps/ios/          iOS 平台入口(staticlib)。只验证编译,打包需 macOS
apps/web/          Web 平台入口(cdylib + wasm-bindgen)。只验证编译
server/            开发用 axum 服务端,与客户端共享 contract
docker/            Docker 构建工作流(给没有 nix 的机器);见 docker/README.md
Android.nix        NixOS 本机原生工具链(nix-shell)
xtask/             构建逻辑(`cargo xtask android`),容器/本机通用
docs/note/build-apk.md  APK 构建全流程与编译逻辑详解
```

`app-core` 不知道 `api` 的存在:网络由 `ui` 注入。这既让领域逻辑能脱离网络单测,
也让 `Send` 约束被关在 `api` 内部,`app-core` 因此能原样编到 wasm。见
[`docs/adr/0002`](docs/adr/0002-send-boundary-lives-inside-api-crate.md)。

裸 `cargo build` 只构建桌面链路(workspace 的 `default-members`)。
android / web / ios / server 一律靠 `-p` 显式构建 —— 因为 `android_main` 在宿主机
target 上根本编不过。

## 跑通客户端-服务端往返

```sh
just server-dev              # 终端 1:axum,监听 127.0.0.1:3000
just desktop-dev             # 终端 2:桌面端,点「Check server」
just server-test             # 终端 2:或者直接打一次真实请求
```

Android 真机:

```sh
just server-dev              # 终端 1
just android-build           # 终端 2
just android-run             # 装 APK + adb reverse + 看日志
```

手机上的 `127.0.0.1` 指的是**手机自己**,所以必须 `adb reverse tcp:3000 tcp:3000`
把它转发到开发机(`just android-run` 已包含这一步,adb 重连后需重新执行)。
另外 Android 9 起默认禁止明文 HTTP,`usesCleartextTraffic` 只在 debug 变体的
manifest 里打开。

manifest 还必须声明 `INTERNET` 权限。Android 内核把 `AF_INET` socket 的**创建**权限
绑在 `AID_INET`(gid 3003)上,而该组由这条权限授予 —— 没有它,`socket()` 直接 EACCES,
连 `bind` 到 `127.0.0.1` 都不行,不只是访问外网。

## 为什么有 Java 代码?

UI 是 100% Slint,逻辑是 100% Rust。`apps/android/gradle/` 目录下唯一的 Java 文件
`MainActivity.java`(约 30 行)**不是** UI 或业务代码,而是 Android 平台
强制要求的入口存根:每个 App 都必须有一个 `Activity` 作为系统启动入口,
Rust/Slint 渲染出的画面正是被这个 Activity 加载的 native `.so` 绘制的。

它继承自系统的 `NativeActivity`,只做一件事——`setupEdgeToEdge()`,把窗口
铺成全面屏,让 Slint UI 能画到状态栏和导航栏底下(配合 `app.slint` 里的
`safe-area-insets`)。其余的 `AndroidManifest.xml`、`res/` 资源、`*.gradle`
都是标准的 Android 打包配置,不含任何应用逻辑。

> 如果不需要全面屏,可以删掉 `MainActivity.java`,把 manifest 里的
> `android:name=".MainActivity"` 换成系统自带的 `android.app.NativeActivity`,
> 即可做到零 Java——代价是 UI 不再延伸到系统栏后面。本项目选择保留全面屏。

## 构建 APK

两条路,产物一致,按环境选一条:

```sh
./docker/build.sh      # Docker:给没有 nix 的机器/CI
just android-build     # NixOS 本机原生:更快、无镜像开销(nix-shell Android.nix)
adb install -r dist/slint-study-debug.apk
```

为模拟器(x86_64)或多个 ABI 构建:

```sh
ABIS="x86_64" just android-build           # 或 ABIS="x86_64" ./docker/build.sh
ABIS="arm64-v8a armeabi-v7a x86_64" just android-build
```

- Docker 路径细节见 [`docker/README.md`](docker/README.md);
- 完整编译流程见 [`docs/note/build-apk.md`](docs/note/build-apk.md)。

`settings.gradle` 把阿里云的 maven 镜像排在 `google()` / `mavenCentral()` 之前:直连
Google Maven 时 gradle 会挑到国内被黑洞的 IP,每次构建白等几分钟 TCP 超时(实测同一个
AGP pom,阿里云 0.05s,直连 3.9s;整体构建 6min → 2min46s)。清华和 USTC 没有 Maven
镜像,故不可用。原仓库保留在后面作兜底。

## 桌面开发构建

在桌面上运行同一套 UI(需要 `libfontconfig1-dev`):

```sh
cargo run -p app-desktop
```

如果需要热重载 UI,改用 `just desktop-dev`:它启用了 Slint 的 `live-preview`
特性,编辑 `crates/ui/slint/*.slint` 会直接刷新运行中的窗口,无需重新编译或
重启(Rust 逻辑会保留;修改 Rust 代码仍需重启)。

```sh
just desktop-dev                        # 热重载
SLINT_STUDY_FPS=1 just desktop-dev      # 外加左上角帧率读数
```

`SLINT_STUDY_FPS` 是运行期开关,拨动它不必重新编译。读数是**诚实的即时帧率**:
只数渲染通知里真实发生的帧,刻意不主动请求重绘,所以空闲时会自己趴到 ~1,
交互和动画时才飙上去。关掉时连采样定时器都不建。

wasm 与 APK 读不到运行期环境变量(页面由浏览器拉起、APK 由系统拉起),那两端
这个开关在构建期生效,得带着它重新构建:`SLINT_STUDY_FPS=1 just web-dev`。

## 让 AI 助手看见运行中的界面(MCP)

Slint 1.17 起,MCP server 可以**编译进应用自身**(`slint/mcp` feature)。开启后 AI 助手
不再靠猜或截图,而是直接读运行中窗口的元素树、模拟点击和键盘输入。
背景见 [`docs/slint/slint-and-ai-mcp.md`](docs/slint/slint-and-ai-mcp.md)。

```sh
just mcp-desktop      # 桌面端 + MCP,监听 127.0.0.1:8090
just mcp-desktop-3d   # 同上,外加 bevy 3D 页(那页的热调面板最值得给 AI 看)
```

客户端那一侧由仓库根的 [`.mcp.json`](.mcp.json) 声明(名为 `slint-app`),
Claude Code 等助手会自动挂接。**必须先把 app 跑起来** —— MCP server 活在应用进程里,
app 没跑就连不上。

Android 真机:

```sh
just mcp-android      # 烧入端口重编 APK + 装机 + adb forward + 启动
```

这里用的是 `adb forward` 而非 `adb reverse`:MCP server 跑在**手机**里,是开发机要连
进去,方向与前面转发 `server-dev` 的那次相反。

> **玩完手机记得撤转发**:`adb forward` 会一直占着 8090。之后再跑 `just mcp-desktop*`,
> slint 绑不上端口时**只在日志里留一行 `Address already in use` 就继续跑**,app 一切正常
> —— 而 AI 客户端按 `.mcp.json` 连 127.0.0.1:8090,连上的是**手机里的旧 APK**,读到的
> 元素树和截图全是手机的,浑然不觉。踩过一次,查了半天。
>
> 现在 `mcp-desktop*` 前置了一道端口守卫,占用时直接失败并点名占用者,不会再静默降级。
> 撤转发:`adb forward --remove tcp:8090`。

Slint 1.17 本身在 android 上**没接** MCP:桌面走 `i-slint-backend-selector`,它设完
platform 会顺手调 `mcp_server::init()`;而 `slint::android::init()` 直接调
`set_platform`,把 selector 整个绕过去,那个钩子永远不触发。`apps/android/src/lib.rs`
里手动补了这一刀(代价是直依赖 `i-slint-backend-testing`)。上游哪天接上了就能删掉。

### 三个开关,少一个都白搭

| 开关 | 时机 | 不设的后果 |
|------|------|-----------|
| `--features mcp` | 构建期 | 根本没有 server |
| `SLINT_EMIT_DEBUG_INFO=1` | 构建期 | **静默地瞎**:server 正常起、工具正常列,但 `get_element_tree` 只回一个没有类型名、没有子节点的空壳根 |
| `SLINT_MCP_PORT` | 运行期(android 为构建期) | 不监听,零开销 |

中间那条是最容易踩的坑:它不报错、不警告,只是让 AI 看到一片空白。`just mcp-*` 已经
把三个全备齐了,手敲命令时才需要留意。

android 侧 `SLINT_MCP_PORT` 是构建期的 —— APK 由系统启动,进程拿不到运行时环境变量,
只能靠 `apps/android/src/lib.rs` 里的 `option_env!` 编进二进制。端口在 `justfile` 的
`mcp_port` 里定义一次,改了要同步 `.mcp.json`。

`mcp` feature 默认关闭:它会把测试后端和软件渲染器编进二进制。别跑 `--all-features`,
那会把它拖进来。

## License

GPL-3.0,外加一条附加限制:OpenAI 及其关联公司不获得本许可证的任何授权
(含用作模型训练数据或推理输入)。详见 [LICENSE](LICENSE)。
