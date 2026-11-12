# 开发工作流

日常开发要用的命令,以及每条命令背后那些不写下来就会重踩的坑。
README 只留最短跑通路径,细节都在这里。

## 桌面

```sh
cargo run -p app-desktop                # 普通运行(需要 libfontconfig1-dev)
just desktop-dev                        # 热重载
OSMOSIS_FPS=1 just desktop-dev      # 外加左上角帧率读数
```

`just desktop-dev` 启用了 Slint 的 `live-preview` 特性:编辑 `crates/ui/slint/*.slint`
会直接刷新运行中的窗口,无需重新编译或重启(Rust 逻辑会保留;改 Rust 代码仍需重启)。

`OSMOSIS_FPS` 是运行期开关,拨动它不必重新编译。读数是**诚实的即时帧率**:
只数渲染通知里真实发生的帧,刻意不主动请求重绘,所以空闲时会自己趴到 ~1,
交互和动画时才飙上去。关掉时连采样定时器都不建。

wasm 与 APK 读不到运行期环境变量(页面由浏览器拉起、APK 由系统拉起),那两端这个开关
在构建期生效,得带着它重新构建:`OSMOSIS_FPS=1 just web-dev`。

量「点一下冻多久」用 `OSMOSIS_STALL`,开关规矩同上(APK 要构建期带上)。它不看均值:
一个 10ms 的定时器两次触发之间隔了超过 50ms,就记一行 `ui: 主线程卡了 Nms`,
点一下之后那几行加起来就是这一下的冻结时长。量要用 release 档 —— debug 档的本地
crate 是 opt-level 0,数字会失真。

截真实窗口像素:

```sh
just shot        # 宽版式
just shot 420    # 紧凑版式;第二个参数指定开局页,如 `just shot 420 1` 直接进 Music 页
```

## 本地 PostgreSQL 与网络隔离

`just pg` 把容器的 `/var/run/postgresql` 挂到
`${XDG_STATE_HOME:-$HOME/.local/state}/osmosis-postgres/socket`，数据仍在
`osmosis-pgdata` 命名卷里。父目录权限为 `0700`，只允许当前用户访问；容器入口会调整
socket 子目录的属主，不要手动改它。宿主的 `127.0.0.1:5432` 也继续保留。

Claude 的网络命名空间有自己的回环网卡，不能直连宿主的 5432；文件系统路径可共享，
所以 `just server-dev` 默认通过 Unix socket 连接。显式设置的 `DATABASE_URL`
（包括 `.env` 里的值）优先，不会被覆盖。`just pg-url` 输出 socket 连接串：

```sh
just server-dev
# 单独验证数据库，或给裸 cargo 命令指定同一个开发库：
psql "$(just pg-url)" -c 'select 1'
DATABASE_URL="$(just pg-url)" cargo test -p server
```

URL 形如 `postgres://slint:devonly@localhost/osmosis?host=%2Fhome%2F…%2Fsocket`。
`host` 参数是 socket **目录**的绝对路径，经过 URL 编码；sqlx 不会为这个连接发起 TCP
或 DNS 请求。上述测试命令留给需要测试时使用，日常开发仍只跑 `just agent-check`。

旧容器不会因 `docker start` 自动得到新挂载。`just pg` 检测不到共享 socket 会立即报错，
不让服务端等到 `PoolTimedOut`。确认没有其他任务使用开发库后，迁移一次：

```sh
docker stop osmosis-pg
docker rename osmosis-pg osmosis-pg-before-unix-socket
just pg
psql "$(just pg-url)" -c 'select 1'
```

新容器沿用原来的 `osmosis-pgdata`，不删卷、不初始化另一份数据。旧容器保持停止，
不能与新容器同时使用该卷；验证成功后可用 `docker rm osmosis-pg-before-unix-socket`
清理旧容器。连接串与挂载路径应使用同一用户、同一 `XDG_STATE_HOME`；改变它后也要重建容器。

这只解决 PostgreSQL 连接；bang-dream 的外部 DNS、HTTP 请求仍遵循各自的代理配置。

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
adb install -r dist/osmosis-debug.apk
```

为模拟器(x86_64)或多个 ABI 构建:

```sh
ABIS="x86_64" just android-build           # 或 ABIS="x86_64" ./docker/build.sh
ABIS="arm64-v8a armeabi-v7a x86_64" just android-build
```

- Docker 路径细节见 [`docker/README.md`](../../docker/README.md);
- 完整编译流程见 [`build-apk.md`](build-apk.md)。

用户在洛杉矶,深圳有几台 VPS,proxy 是给这些 VPS 用的。代理配置或广东出口 IP 不代表
用户本人或本机在国内,也不能据此确定历史测试的执行位置。

`settings.gradle` 把阿里云的 maven 镜像排在 `google()` / `mavenCentral()` 之前:当时直连
Google Maven 时 gradle 会挑到连不上的 IP,每次构建白等几分钟 TCP 超时(记录中同一个
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
just desktop-dev      # 桌面端,MCP 默认开在 127.0.0.1:8091
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

桌面(8091)与真机(8090)各占一个端口,两边可以同时开着。`.mcp.json` 里 `slint-app` 指桌面、
`slint-android` 指手机。slint 绑不上端口时只在日志里留一行 `Address already in use` 就继续跑,
所以 `just desktop-dev*` 前置了端口守卫,占用时直接失败并点名占用者。

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

## 验证的坑

这些都是静默失败:不报错,只让人误判「改动没生效」或「功能不可用」。

### 实例与进程

- 包名 `app-desktop`,进程名 `osmosis-desktop`。拿包名去 `pkill`、去跑 `target/debug/app-desktop`
  都不报错,只是没杀掉、没启动。`pkill -f app-desktop` 还会匹配到自己那条命令行,连父 shell
  一起杀掉。用 `just desktop-kill`(按进程名精确匹配)。
- 旧实例没退干净时,截图、元素树读到的都是旧进程。
- `cargo build | tail` 会把输出攒到进程结束才吐,中途看起来和吊死一样;全量重定向进文件再读。
  `ps`/`pgrep` 在这台机上的结果不可单独采信;判死活看任务完成通知、日志在长、产物时间戳变新。
- 首次进 nix 环境(尤其 `Android.nix` 的 gradle)可能静默下载几百 MiB。
- 两个构建抢同一个产物目录时,后完成的覆盖前者,时间戳和日志都看不出异常。测量前核对产物
  时间戳。

### 截图与像素

- `just shot [宽度]` 的产物 `dist/shot.png` 是无头 sway 合成器的输出(窗口高固定 900),基准图
  只在无头下生成。`just desktop-dev` 起着的会话用
  `bun test/headless/headless.ts capture --dir <会话目录> --out a.png` 抓当前画面。
- MCP 的 `take_screenshot` 采得到导入的 wgpu 纹理,判「画出来了没有」够用;颜色与合成的逐像素
  比对以 `just shot` 为准。真机上它报 Skia 包装错误,改用 `adb shell screencap`(物理像素,
  逻辑坐标乘 scale)。
- 选探针前确认它头顶没有东西:侧栏和光带按钮被 `nav-bg` 等导入纹理盖着,而这些纹理只在 GPU
  构建里非空,于是「非 GPU 构建生效、GPU 构建不生效」可能只是探针被盖住。内容区的实底元素安全。
- 量像素用 `magick dist/shot.png -format "%[pixel:p{x,y}]" info:`,不看观感。看不出效果时先确认
  取景(移轴镜头、当前效果是哪一个),再怀疑管线。(曾经这里记的是「遮挡层看卡片边框」,
  标注卡与遮挡层已随 `docs/adr/0041` 删除。)
- 元素在不在,以 `crates/ui/tests/` 里 `find_by_element_id` 为准;MCP 只证明「你连的那份树里没有」。
  会话启动时桌面没跑,工具列表里只剩 `slint-android`(1080x2400、scale 2.75),很容易当成桌面;
  拿 `adb shell wm size` 对一下。
- MCP 客户端没连上时,端点就是普通 HTTP:`POST http://127.0.0.1:8091/mcp` 发 JSON-RPC
  (`Accept: application/json, text/event-stream`),参数名 camelCase。
- 端到端脚本按元素 id 找控件,断言落在库里的行、量出的尺寸或状态上;样板是 `test/pick-e2e.sh`。

### `.slint` 热重载

- `SLINT_LIVE_PREVIEW` 是构建期变量(slint 编译器读它决定生成哪套代码),只在运行时带上则产物
  里没有热重载,改 `.slint` 毫无反应。`just desktop-dev` 一条命令覆盖构建与运行。
- 文件监听器收不到 `sed -i`(写临时文件再 rename,inode 变了);要原地截断写。
- 判据是日志里的 `Reloaded component MainWindow from <路径>`。惰性渲染下没有输入就不重绘。

### 真机

- 开发装机走 `just mcp-android`(debug 档,MCP 读得到元素树);发布件走 `just android-build`。
  `target-android/` 按 profile 分树。仓库改过路径后旧树的 `CMakeCache.txt` 记着老路径,走 cmake
  的 `-sys` crate 会编不过,删掉那棵树。
- 安卓配方的 adb 都带序列号:多台在线又没设 `ANDROID_SERIAL` 时拒绝;debug 包按
  `ro.product.model` 拒装到生产平板。`adb reverse`/`forward` 挂在 adb 连接上,无线 adb 换端口或
  拔插后要重跑 `just mcp-android`(或 `just mcp-forward android-reverse`),否则启动即信令错误。
- 锁屏时 `screencap` 出 0 字节、`get_window_properties` 的 `size` 为空,那不是 MCP 坏了。解锁:

  ```sh
  set -a; source .env; set +a
  adb shell input keyevent KEYCODE_WAKEUP
  adb shell input swipe 540 2200 540 600 200   # 不上滑 PIN 键盘不出来
  adb shell input text "$ANDROID_DEVICE_PIN"
  adb shell input keyevent KEYCODE_ENTER
  ```

  `wm dismiss-keyguard` 过不去。验证期间 `adb shell svc power stayon usb`,收尾 `stayon false`。
- 服务端会把旧会话判失效,应用停在登录页。走 MCP 填 `LoginPage::username` /
  `LoginPage::password`(值取 `.env` 的 `TEST_USERNAME` / `TEST_PASSWORD`),按无障碍标签「登录」点提交。
- MIUI 的 `INSTALL_FAILED_USER_RESTRICTED` 多半不是开关关了(`getprop persist.security.adbinstall`
  为 `1` 就是开着),而是安全中心的确认页(`AdbInstallActivity`)十来秒没人点就按取消处理。无线 adb
  上一次 `uiautomator dump` 往返约 2.3s,host 端轮询追不上,检测和点击要在手机上用 `sh` 跑;bounds
  只能从匹配到的那个 `<node>` 里取:

  ```sh
  X=$ANDROID_SERIAL
  adb -s "$X" shell input keyevent KEYCODE_WAKEUP
  adb -s "$X" install -r dist/osmosis-debug.apk & ip=$!
  adb -s "$X" shell sh -c '
  for i in $(seq 1 40); do
    dumpsys window | grep -m1 mCurrentFocus | grep -q AdbInstallActivity && {
      uiautomator dump /sdcard/ui.xml >/dev/null 2>&1
      node=$(grep -oE "<node[^>]*text=\"(继续安装|安装)\"[^>]*>" /sdcard/ui.xml | head -1)
      bounds=$(echo "$node" | grep -oE "bounds=\"\[[0-9]+,[0-9]+\]\[[0-9]+,[0-9]+\]\"" | grep -oE "[0-9]+")
      set -- $bounds
      [ $# -eq 4 ] && { input tap $(( ($1+$3)/2 )) $(( ($2+$4)/2 )); break; }
    }
    sleep 1
  done
  '
  wait $ip
  ```
