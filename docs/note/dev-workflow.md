# 开发工作流

日常开发要用的命令,以及每条命令背后那些不写下来就会重踩的坑。
README 只留最短跑通路径,细节都在这里。

## 桌面

```sh
cargo run -p app-desktop                # 普通运行(需要 libfontconfig1-dev)
just desktop-dev                        # 热重载
SLINT_STUDY_FPS=1 just desktop-dev      # 外加左上角帧率读数
```

`just desktop-dev` 启用了 Slint 的 `live-preview` 特性:编辑 `crates/ui/slint/*.slint`
会直接刷新运行中的窗口,无需重新编译或重启(Rust 逻辑会保留;改 Rust 代码仍需重启)。

`SLINT_STUDY_FPS` 是运行期开关,拨动它不必重新编译。读数是**诚实的即时帧率**:
只数渲染通知里真实发生的帧,刻意不主动请求重绘,所以空闲时会自己趴到 ~1,
交互和动画时才飙上去。关掉时连采样定时器都不建。

wasm 与 APK 读不到运行期环境变量(页面由浏览器拉起、APK 由系统拉起),那两端这个开关
在构建期生效,得带着它重新构建:`SLINT_STUDY_FPS=1 just web-dev`。

截真实窗口像素:

```sh
just shot        # 宽版式
just shot 420    # 紧凑版式;第二个参数指定开局页,如 `just shot 420 1` 直接进 Music 页
```

## 同播:在一台机器上验证

设备身份是主机名加进程号,所以同机的两个实例天然是两台设备:

```sh
just server-dev                             # 终端 1:信令跟着后端一起起,不必单独开
just desktop-dev                            # 终端 2:实例 A
just --set mcp_port 8091 desktop-dev        # 终端 3:实例 B,换个 MCP 端口免得撞
```

界面上只有两个动作:音乐页放一首歌,控制条右侧常驻同播区 —— 在线设备列成小胶囊,
点一台就推过去;一台都没有时写「同播: 没有其他设备」,功能始终可见。

推流链路本身有对着真信令服务端跑的测试(`crates/syncplay/tests/client.rs`):
两个客户端连上、互相看见、推一路正弦波过去,断言收到的采样**有能量**。
界面之下的一切都在那条测试里,界面本身只剩「把事件搬到 Slint 的模型上」。

## 音乐链路的开发细节

bang-dream 是独立仓库,不挂在本仓库里。跑之前先告诉 just 它在哪
(不设时默认找同级目录的 `../bang-dream`):

```sh
BANG_DREAM_REPO=~/projects/bang_dream just bang-dream
```

`cargo xtask boundaries` 也读这个变量:设了就拿上游的 `.proto` 跟 `server/proto`
那份副本比对,没设就跳过 —— CI 上没有上游,这是常态。

axum 侧的上游地址由 `BANG_DREAM_ADDR` 覆盖。

**已知缺口:web 端歌名是豆腐块。** 搜索结果里的歌名是任意中日文,不可能预裁进内嵌的
子集字体(`just font-subset` 只裁硬编码文案里出现过的字),所以那两列**不指定字体**,
落到系统字体。linux 与安卓都有系统 CJK,而 wasm 里没有系统字体。等 web 端真正落地时
一并解决。

## Android 真机

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

- Docker 路径细节见 [`docker/README.md`](../../docker/README.md);
- 完整编译流程见 [`build-apk.md`](build-apk.md)。

`settings.gradle` 把阿里云的 maven 镜像排在 `google()` / `mavenCentral()` 之前:直连
Google Maven 时 gradle 会挑到国内被黑洞的 IP,每次构建白等几分钟 TCP 超时(实测同一个
AGP pom,阿里云 0.05s,直连 3.9s;整体构建 6min → 2min46s)。清华和 USTC 没有 Maven
镜像,故不可用。原仓库保留在后面作兜底。

### 为什么有 Java 代码?

UI 是 100% Slint,逻辑是 100% Rust。`apps/android/gradle/` 下唯一的 Java 文件
`MainActivity.java`(约 30 行)是 Android 平台强制要求的入口存根:每个 App 都必须有一个
`Activity` 作为系统启动入口。它继承自 `NativeActivity`,只做一件事 —— `setupEdgeToEdge()`,
把窗口铺成全面屏,让 Slint UI 能画到状态栏和导航栏底下(配合 `app.slint` 里的
`safe-area-insets`)。

不需要全面屏的话可以删掉它,把 manifest 里的 `android:name=".MainActivity"` 换成系统自带的
`android.app.NativeActivity`,即可做到零 Java —— 代价是 UI 不再延伸到系统栏后面。
本项目选择保留全面屏。

## 让 AI 助手看见运行中的界面(MCP)

Slint 1.17 起,MCP server 可以**编译进应用自身**(`slint/mcp` feature)。开启后 AI 助手
不再靠猜或截图,而是直接读运行中窗口的元素树、模拟点击和键盘输入。
背景见 [`../slint/slint-and-ai-mcp.md`](../slint/slint-and-ai-mcp.md)。

**开发链路默认开着**:`desktop-dev` 已经带上 feature 与两个环境变量。
发布产物不带 —— `mcp` 不在 `apps/desktop` 的 `default` feature 里,`cargo build --release`
与 APK 都是干净的。这个区分是有意的:MCP 等于把「读完整 UI 树 + 截图 + 合成点击」
开给 localhost 上的任何进程,开发时值,发出去不值。

```sh
just desktop-dev      # 桌面端,MCP 默认开在 127.0.0.1:8090
```

客户端那一侧由仓库根的 [`.mcp.json`](../../.mcp.json) 声明(名为 `slint-app`),
Claude Code 等助手会自动挂接。**必须先把 app 跑起来** —— MCP server 活在应用进程里,
app 没跑就连不上。

Android 真机:

```sh
just mcp-android      # 烧入端口重编 APK + 装机 + adb forward + 启动
```

这里用的是 `adb forward` 而非 `adb reverse`:MCP server 跑在**手机**里,是开发机要连
进去,方向与转发 `server-dev` 的那次相反。

> **玩完手机记得撤转发**:`adb forward` 会一直占着 8090。之后再跑 `just desktop-dev*`,
> slint 绑不上端口时**只在日志里留一行 `Address already in use` 就继续跑**,app 一切正常
> —— 而 AI 客户端按 `.mcp.json` 连 127.0.0.1:8090,连上的是**手机里的旧 APK**,读到的
> 元素树和截图全是手机的,浑然不觉。踩过一次,查了半天。
>
> 现在 `desktop-dev*` 前置了一道端口守卫,占用时直接失败并点名占用者,不会再静默降级。
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

中间那条是最容易踩的坑:它不报错、不警告,只是让 AI 看到一片空白。`just desktop-dev*`
与 `just mcp-android` 已经把三个全备齐了,手敲命令时才需要留意。

android 侧 `SLINT_MCP_PORT` 是构建期的 —— APK 由系统启动,进程拿不到运行时环境变量,
只能靠 `apps/android/src/lib.rs` 里的 `option_env!` 编进二进制。端口在 `justfile` 的
`mcp_port` 里定义一次,改了要同步 `.mcp.json`。

`mcp` feature 默认关闭:它会把测试后端和软件渲染器编进二进制。别跑 `--all-features`,
那会把它拖进来。
