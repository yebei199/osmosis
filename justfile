apk := "dist/slint-study-debug.apk"
# 应用内嵌 MCP server 的端口(见 mcp-* 配方与 .mcp.json)。web-dev、server-dev 各占
# 一个(见下),故取 8090。改这里就得同步改 .mcp.json —— 那是 AI 客户端那一侧的地址。
mcp_port := "8090"
# web-dev 静态服务器的端口。刻意避开 8080/8000 这类烂大街的号:那些常年被别的项目
# 的 dev server 占着,撞上了只会得到一句 Address already in use。
web_port := "8073"

# 无参数时列出全部配方(按 group 分组),而非直接跑第一条
_default:
    @just --list

# 本地跑一遍 CI 会跑的全部检查。dev 上的 push 不触发 CI,提交前跑这个
# 与 .github/workflows/ci.yml 一一对应,命令逐字相同
[group('ci')]
ci: ci-test ci-cross ci-boundaries
    @echo "==> CI 全部通过"

# 桌面链路:单测 + clippy(-D warnings,和 CI 一致)
#
# **一条命令都不设 RUSTFLAGS**。它进 cargo 的构建指纹,而且是整棵依赖树的 ——
# 设了它,`just ci` 与 `just desktop-dev` 各自维护一套 500 多个 crate 的产物,
# 来回切就是来回冷编。warning 的拦截改由 clippy 的 `-- -D warnings` 承担:
# 那是传给最终 crate 的参数,不进依赖指纹,且 clippy 本就涵盖全部 rustc lint。
[group('ci')]
ci-test:
    nix-shell slint.nix --run 'cargo test'
    # 能力层与服务端都不在 default-members 里(它们由 ui 注入,不是它的依赖树入口),
    # 裸 `cargo test` 只编不测。不点名的话,同播那三条端到端测试一次都不会跑。
    nix-shell slint.nix --run 'cargo test -p audio -p syncplay -p server -p xtask'
    nix-shell slint.nix --run 'cargo clippy --all-targets -- -D warnings'
    nix-shell slint.nix --run 'cargo clippy --all-targets -p audio -p syncplay -p server -p xtask -- -D warnings'

# 本地跑不动的端,至少保证能编译。android 的 build.rs 要 platform jar,故走 Android.nix
[group('ci')]
ci-cross:
    nix-shell slint.nix --run 'cargo check -p app-web --target wasm32-unknown-unknown'
    nix-shell slint.nix --run 'cargo check -p app-ios --target aarch64-apple-ios'
    nix-shell Android.nix --run 'cargo check -p app-android --target aarch64-linux-android'

# 架构边界(docs/adr/0001、0002)。与 CI 调的是同一份 xtask 代码
[group('ci')]
ci-boundaries:
    nix-shell slint.nix --run 'cargo xtask boundaries'

# 热重载 UI 开发:编辑 crates/ui/slint/*.slint 保存即刷新运行中的窗口(改 Rust 逻辑仍需重启)
# 左上角帧率读数:`SLINT_STUDY_FPS=1 just desktop-dev`(运行期开关,不必重编)
#
# **MCP 默认开着**。调试与验证一律走它 —— 读元素树、模拟点击、量真实尺寸,
# 都比对着截图猜可靠。三个开关缺一不可,所以焊在这条配方里而不是让人记:
# feature `mcp`、构建期 SLINT_EMIT_DEBUG_INFO、运行期 SLINT_MCP_PORT。
# 其中调试信息现在由 crates/ui/build.rs 在 debug 档一律打开(元素树少了它就是空的,
# 且不报错),这里保留显式设置只为把三样凑齐、一眼看得全。
# 发布产物不受影响:`cargo build --release` 与 APK 都不带 mcp(见 apps/desktop 的 features)。
[group('三端')]
[group('桌面')]
desktop-dev extra="": mcp-port-free
    SLINT_EMIT_DEBUG_INFO=1 SLINT_LIVE_PREVIEW=1 nix-shell slint.nix --run 'SLINT_MCP_PORT={{mcp_port}} cargo run -p app-desktop --features mcp,slint/live-preview{{ if extra != "" { "," + extra } else { "" } }}'

# 网页版:编译 wasm + 生成胶水代码 + 起静态服务器,浏览器开 http://127.0.0.1:8073(见 web_port)
# 本命令自带服务端,不必另开终端 —— 「Check server」开箱即通。
# 无热重载(浏览器加载的是打包产物),改完代码重跑本命令并刷新页面。
# 用 release:debug 的 wasm 有上百 MB,浏览器加载能等到天荒地老。
# 左上角帧率读数:`SLINT_STUDY_FPS=1 just web-dev` —— wasm 读不到运行期环境变量,
# 这个开关在**构建期**生效,故必须重跑本命令。
[group('三端')]
web-dev:
    nix-shell slint.nix --run 'cargo build -p app-web --target wasm32-unknown-unknown --release'
    nix-shell slint.nix --run 'wasm-bindgen target/wasm32-unknown-unknown/release/app_web.wasm --target web --no-typescript --out-dir dist/web'
    cp apps/web/index.html dist/web/
    # 手工排查用的静态页(见 test/README.md)。跟着一起发,省得每次另起服务器。
    cp test/*.html dist/web/
    # server 不在 default-members 里,裸 cargo build 从不编它。先编完再起,否则页面
    # 已经能开、按钮却要再等半分钟才通,报的还是「网络错误」,徒增困惑。
    nix-shell slint.nix --run 'cargo build -p server'
    # 上次 Ctrl-C 没杀干净的 server 还占着端口,先收尸。不自己 pkill:web-stop 带 curl 复核,
    # 端口没让出来就直接失败,免得新服务器绑不上静默退出、浏览器却还连着喂旧产物的老进程。
    just web-stop
    # server 放后台、静态服务器占前台,Ctrl-C 走 trap 把 server 一起带走。
    # 3000 被占说明已有一个 server 在跑(比如 android 调试用的那个),这个会 panic
    # 退出、不影响前端,那边的链路也毫发无伤。不为此发明端口探测。
    # 不用 `python -m http.server`:它不发 Cache-Control,浏览器会按启发式新鲜度缓存
    # 那个 36MB 的 wasm,重新构建后页面还在跑旧产物,症状与代码 bug 无法区分。
    # dev-server.py 只是在它基础上加了 no-store,其余行为一致(含绑回环的理由)。
    nix-shell slint.nix --run 'cargo run -p server & trap "kill %1 2>/dev/null" EXIT; python3 apps/web/dev-server.py {{ web_port }} dist/web'

# 只杀本项目这个端口上的 dev server,不误伤别人的 python。[.] 是为了让这行自己的命令行
# 不被这个正则匹配上 —— 否则 pkill 会连本 recipe 一起杀(见 pkill -f 打到自己那次)。
# 模式必须跟 web-dev 起服务那行**逐字对应**,对不上就杀不掉。
# 收掉 web-dev 留下的后台进程,并确认端口真的没人应答了
[group('三端')]
web-stop:
    pkill -f 'dev-server[.]py {{ web_port }}' || true
    # pkill 只发信号,不等进程退出,退出码也只说"匹配到了几个",不说端口有没有让出来。
    # 只有 curl 连不上才算真关掉:模式写错、还有第二个实例、进程赖着不死,都在这里现原形。
    for i in 1 2 3 4 5 6; do curl -sf -o /dev/null --max-time 1 http://127.0.0.1:{{ web_port }}/ || exit 0; sleep 0.5; done; echo "端口 {{ web_port }} 仍在应答,没关掉" >&2; exit 1

# 重裁中文子集字体。slint 内嵌的 Inter 没有汉字,wasm 上又没有系统字体可回退,
# 所以 crates/ui/slint/app.slint 用 `import` 内嵌这份子集(20MB → 28KB)。
# 改了界面上的中文文案后跑一遍,把新字符补进 --text —— 注意 api::ApiError 的
# Display 也会原样显示到界面上。ASCII 一并裁入,好让中英混排的那行文案整体落在
# 同一个字体上,不依赖逐字形回退。
# 漏字不会静默:cargo test -p ui 的 describe_only_uses_subset_glyphs 会报出缺哪个字。
# name-IDs 13/14 是 OFL 的许可声明,必须随子集一起分发,故显式保留。
[group('工具')]
font-subset:
    nix-shell -p python3Packages.fonttools --run "pyftsubset \
      $(fc-match -f '%{file}' 'Maple Mono NF CN:style=Regular') \
      --text='未查询中服务端协议失败网络错误响应格式版本不匹配本机·…,点一首歌开加载正在播放音频设备流解码始端暂不支持同播推给台收听连接上信令载荷线消息队列完了没有其他' \
      --unicodes=U+0020-007E \
      --layout-features= --no-hinting --desubroutinize \
      --notdef-outline --name-IDs+=13,14 \
      --output-file=crates/ui/fonts/cjk-subset.ttf"
    @ls -la crates/ui/fonts/cjk-subset.ttf

# 起本地 Postgres(容器)。server-dev 与 `cargo test -p server` 都要它。
# 数据在命名卷里,容器删了也还在。
pg:
    docker start slint-study-pg 2>/dev/null || \
      docker run -d --name slint-study-pg \
        -e POSTGRES_PASSWORD=devonly -e POSTGRES_USER=slint -e POSTGRES_DB=slint_study \
        -p 127.0.0.1:5432:5432 -v slint-study-pgdata:/var/lib/postgresql/data \
        postgres:17-alpine
    @docker exec slint-study-pg sh -c 'until pg_isready -U slint -d slint_study >/dev/null 2>&1; do sleep 0.2; done'

# 开发服务端,监听 127.0.0.1:3000。「Check server」按钮打的就是它
[group('服务端')]
server-dev:
    cargo run -p server

# 起 bang-dream 音乐聚合层(gRPC,127.0.0.1:50051)。server-dev 依赖它。
# 源码是 third_party/bang-dream 这个 submodule,但那里只当契约来源用 ——
# 开发时跑的是 BANG_DREAM_REPO 指向的工作副本,默认为 submodule 自身。
[group('服务端')]
bang-dream repo=env('BANG_DREAM_REPO', 'third_party/bang-dream'):
    cd {{repo}} && go run ./cmd/bang-dream

# 网易云扫码登录,凭据写进 bang-dream 的 data/credentials.json。
# 登录态是全服务一份的,登一次就够,过期了再来
[group('服务端')]
bang-dream-login repo=env('BANG_DREAM_REPO', 'third_party/bang-dream'):
    cd {{repo}} && go run ./cmd/qrlogin

# NixOS 本机原生编译 dist APK(更快、无镜像开销)。前提:已 `rustup default stable`
# ABIS 可选:ABIS="x86_64" just android-build
[group('三端')]
[group('安卓')]
android-build:
    nix-shell Android.nix --run 'CARGO_TARGET_DIR=target-android cargo xtask android'

# USB 直装到手机(推荐:不受移动热点/公司 WiFi 客户端隔离影响)
[group('安卓')]
android-install:
    adb install -r {{apk}}

# 把手机的 127.0.0.1:3000 转发到开发机的 server-dev
# 手机上的 127.0.0.1 指的是手机自己,不转发的话「Check server」永远失败。
# adb 重连后需要重新执行
[group('安卓')]
android-reverse:
    adb reverse tcp:3000 tcp:3000

# 装 APK、接通端口转发,然后看日志。前提:server-dev 已在另一个终端里跑
[group('安卓')]
android-run: android-install android-reverse
    adb shell am start -n io.github.slintstudy/.MainActivity
    adb logcat -s slint_study

# 局域网 http 共享,手机扫码下载
# 可用前提:手机与电脑同一网络且无客户端隔离(如电脑自己开的热点)
# 连的是别人的移动热点/公司 WiFi 多半被隔离,手机连不上,请改用 android-install
[group('安卓')]
android-serve:
    miniserve dist --interfaces 0.0.0.0 --port 3070 --qrcode

# {{mcp_port}} 被占就**立刻失败**,别让 app 起来。
#
# 这是在补一个静默失败:slint 绑不上端口时只在日志里留一行 "failed to bind ...
# Address already in use" 就继续跑,app 一切正常。而 .mcp.json 里 AI 客户端的地址是
# 写死的 127.0.0.1:{{mcp_port}} —— 它会连上**占用者**,并把对方的界面当成你的。
#
# 最容易中招的占用者就是 mcp-forward 留下的 adb forward:AI 于是读到手机里那个旧 APK
# 的元素树和截图,浑然不觉。踩过一次,查了半天。
[private]
mcp-port-free:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! ss -ltn "sport = :{{mcp_port}}" | grep -q LISTEN; then exit 0; fi
    echo "端口 {{mcp_port}} 已被占用,拒绝启动 —— 否则 app 会静默跑在没有 MCP 的状态," >&2
    echo "而 AI 客户端会连到下面这个占用者身上,把它的界面当成你的:" >&2
    ss -ltnp "sport = :{{mcp_port}}" >&2 || true
    if adb forward --list 2>/dev/null | grep -q "tcp:{{mcp_port}}"; then
        echo "" >&2
        echo "是 adb forward 占着(mcp-forward / mcp-android 留下的),连过去看到的是**手机**。" >&2
        echo "解法:adb forward --remove tcp:{{mcp_port}}" >&2
    fi
    exit 1

# 把开发机的 {{mcp_port}} 转发到手机上的同一端口。
# 方向与 android-reverse 相反:MCP server 跑在**手机**里,是开发机要连进去,故用 forward。
# adb 重连后需要重新执行
[group('mcp')]
mcp-forward:
    adb forward tcp:{{mcp_port}} tcp:{{mcp_port}}

# 真机 + MCP:烧入端口重编 APK、装机、接通转发、启动。
# 两个变量在这里**都是构建期**的:APK 由系统启动,进程读不到运行时环境变量,
# 端口只能靠 apps/android/src/lib.rs 里的 option_env! 编进二进制。
# 不带 logcat —— 终端要腾给 AI 会话;要看日志另开一个跑 `adb logcat -s slint_study`
[group('mcp')]
mcp-android: mcp-forward
    nix-shell Android.nix --run 'SLINT_EMIT_DEBUG_INFO=1 SLINT_MCP_PORT={{mcp_port}} FEATURES=mcp CARGO_TARGET_DIR=target-android cargo xtask android'
    adb install -r {{apk}}
    adb shell am start -n io.github.slintstudy/.MainActivity

# 杀掉所有跑着的桌面实例。这条命令踩过三层坑,每一层都**静默失败**:
#
# 1. 进程名是 `slint-study-desktop`([[bin]] name),不是包名 `app-desktop`;
# 2. 它有 19 个字符,而 Linux 的 comm 只存 15 个 —— `pkill -x slint-study-desktop`
#    **永远匹配不到**,只吐一句 warning 就返回 0,看起来像"杀干净了";
# 3. 于是只能用 `-f`(匹配整条命令行),但 `-f` 会连**本命令自己的命令行**一起命中,
#    把调用它的 shell 杀掉,留下退出码 144。方括号 `[s]` 让正则匹配不到字面量本身,
#    这一刀才只落在真正的 app 上。
[group('桌面')]
desktop-kill:
    -pkill -f 'target/debug/[s]lint-study-desktop'

# 关窗后进程是不是干净地走了(issue #15)。开一个实例、关掉、看退出码,要 0 不要 134。
#
# 这条不进 `just ci`:它要合成器给窗口、要显卡给 wgpu adapter,CI 里两样都没有。
# 但凡动过 apps/desktop 的收尾路径、或者升过 wgpu / slint,就在本机跑一次。
[group('桌面')]
desktop-exit-check:
    #!/usr/bin/env bash
    set -uo pipefail
    just desktop-kill
    nix-shell slint.nix --run 'cargo build -p app-desktop'
    nix-shell slint.nix --run 'target/debug/slint-study-desktop' > /tmp/slint-exit-check.log 2>&1 &
    app=$!
    for _ in $(seq 60); do
        id=$(niri msg --json windows | jq -r '.[] | select(.title=="Slint Study") | .id' | head -1)
        [ -n "$id" ] && break
        sleep 1
    done
    [ -n "${id:-}" ] || { echo "窗口没起来,见 /tmp/slint-exit-check.log" >&2; exit 1; }
    sleep 3
    niri msg action close-window --id "$id"
    wait $app
    code=$?
    if [ $code -eq 0 ]; then
        echo "==> 退出码 0,收尾干净"
    else
        echo "==> 退出码 $code(134 = abort),见 /tmp/slint-exit-check.log" >&2
        exit 1
    fi

# 起一个干净的桌面实例,截下**真实窗口像素**,存到 dist/shot.png。
#
# width 给逻辑像素宽度即可切版式:`just shot 420` 看紧凑版(底部导航),`just shot` 看宽版。
# tab 指定开局页(0=Home、1=Music):`just shot 420 1` 直接截紧凑版的 Music 页 ——
# 不必再靠 MCP 模拟点击切页。
#
# 为什么必须有这条 recipe:
# 1. 不先杀干净,旧实例的窗口还在,AI 截到的是**上一版**的界面,浑然不觉;
# 2. MCP 的 take_screenshot 走软件渲染器,**采不到 wgpu 纹理** —— 播放页的粒子与 warp
#    在它眼里恒为纯黑,据此判断「没渲染出来」是错的。真实像素只能靠合成器抓
#    (niri screenshot-window)。
[group('桌面')]
shot width="" tab="0":
    #!/usr/bin/env bash
    set -euo pipefail
    just desktop-kill
    # 先编译再启动:否则「等窗口」的循环会把几分钟的编译时间也等进去,看着像卡死。
    nix-shell slint.nix --run 'cargo build -p app-desktop'
    SLINT_STUDY_TAB={{tab}} nix-shell slint.nix --run 'setsid target/debug/slint-study-desktop' > /tmp/slint-shot.log 2>&1 &
    for _ in $(seq 30); do
        id=$(niri msg --json windows | jq -r '.[] | select(.title=="Slint Study") | .id' | head -1)
        [ -n "$id" ] && break
        sleep 1
    done
    [ -n "${id:-}" ] || { echo "窗口没起来,见 /tmp/slint-shot.log" >&2; exit 1; }
    if [ -n "{{width}}" ]; then
        niri msg action set-window-width --id "$id" {{width}}
        sleep 1
    fi
    # **必须先把窗口摆到眼前**:合成器不给不可见窗口发 frame callback,Slint 就不重绘
    # (fps 读数会是 0),而 screenshot-window 抓的是它最后一次提交的缓冲 —— 于是你看到的
    # 是几次 resize 之前的旧画面,改了相机/布局却"纹丝不动",查半天。
    niri msg action focus-window --id "$id"
    sleep 2
    mkdir -p dist
    # niri 是**异步**落盘的:紧跟着 `ls -t` 会挑到上一张旧图,于是你以为改动没生效,
    # 实际是在看历史。用一个 marker 文件卡住时间,只接受比它新的 png。
    touch /tmp/slint-shot.marker
    niri msg action screenshot-window --id "$id"
    for _ in $(seq 20); do
        shot=$(find ~/Pictures/Screenshots -name '*.png' -newer /tmp/slint-shot.marker | head -1)
        [ -n "$shot" ] && break
        sleep 0.5
    done
    [ -n "${shot:-}" ] || { echo "截图没落盘" >&2; exit 1; }
    mv "$shot" dist/shot.png
    echo "dist/shot.png  ← 窗口 $id,$(niri msg --json windows | jq -r --arg i "$id" '.[]|select(.id==($i|tonumber))|.layout.window_size|join("x")') 逻辑像素"
