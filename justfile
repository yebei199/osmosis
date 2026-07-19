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
[group('ci')]
ci-test:
    nix-shell slint.nix --run 'RUSTFLAGS="-D warnings" cargo test'
    nix-shell slint.nix --run 'RUSTFLAGS="-D warnings" cargo test -p xtask'
    nix-shell slint.nix --run 'RUSTFLAGS="-D warnings" cargo clippy --all-targets'
    nix-shell slint.nix --run 'RUSTFLAGS="-D warnings" cargo clippy --all-targets -p server -p xtask'

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
# 可透传额外 feature,如左上角帧率读数:just desktop-dev debug-fps
[group('三端')]
[group('桌面')]
desktop-dev extra="":
    SLINT_LIVE_PREVIEW=1 cargo run -p app-desktop --features slint/live-preview{{ if extra != "" { "," + extra } else { "" } }}

# 桌面 + 嵌入的 bevy 3D 面板(见 crates/render3d)。走 render3d.nix 拿 vulkan 运行期库。
# 无热重载:bevy 场景在 Rust 里,改完重跑。首帧就绪后窗口中间会出现一个自转的立方体。
[group('桌面')]
desktop-dev-3d:
    nix-shell render3d.nix --run 'cargo run -p app-desktop --features bevy-3d'

# 网页版:编译 wasm + 生成胶水代码 + 起静态服务器,浏览器开 http://127.0.0.1:8073(见 web_port)
# 本命令自带服务端,不必另开终端 —— 「Check server」开箱即通。
# 无热重载(浏览器加载的是打包产物),改完代码重跑本命令并刷新页面。
# 用 release:debug 的 wasm 有上百 MB,浏览器加载能等到天荒地老。
# 可透传额外 feature,如左上角帧率读数:just web-dev ui/debug-fps
[group('三端')]
web-dev extra="":
    nix-shell slint.nix --run 'cargo build -p app-web --target wasm32-unknown-unknown --release{{ if extra != "" { " --features " + extra } else { "" } }}'
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

# 浏览器端到端测试(bun + Playwright,驱系统 Chrome)。静态服务器由 playwright 自己拉起。
# 会弹出一个真实的浏览器窗口并且**必须让它留在前台** —— 被遮住的标签页 rAF 会掉到 1Hz,
# headless 的 Chrome 更是连 WebGPU 都没有。测试自带哨兵,环境不成立时 skip 而非报红。
# 不进 CI,理由同上。详见 test/e2e/README.md。
[group('三端')]
web-test:
    # 测的是产物不是源码:忘了重新 web-dev 就会在旧 wasm 上验新改动,而且看不出异常。
    test -f dist/web/app_web_bg.wasm || { echo "dist/web 里没有产物,先跑 just web-dev bevy-3d" >&2; exit 1; }
    cd test/e2e && bun install --frozen-lockfile && bunx playwright test

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
      --text='未查询中服务端协议失败网络错误响应格式版本不匹配本机·…,' \
      --unicodes=U+0020-007E \
      --layout-features= --no-hinting --desubroutinize \
      --notdef-outline --name-IDs+=13,14 \
      --output-file=crates/ui/fonts/cjk-subset.ttf"
    @ls -la crates/ui/fonts/cjk-subset.ttf

# 开发服务端,监听 127.0.0.1:3000。「Check server」按钮打的就是它
[group('服务端')]
server-dev:
    cargo run -p server

# 打一次真实的 GET /health(需要 server-dev 正在另一个终端里跑)
[group('服务端')]
server-test:
    cargo test -p api -- --ignored

# NixOS 本机原生编译 dist APK(更快、无镜像开销)。前提:已 `rustup default stable`
# ABIS 可选:ABIS="x86_64" just android-build
[group('三端')]
[group('安卓')]
android-build:
    nix-shell Android.nix --run 'CARGO_TARGET_DIR=target-android cargo xtask android'

# 同上,但把 bevy 3D 面板(见 crates/render3d)编进 APK。native 库仍是 release
# profile(bevy debug 产物几百 MB)。装机后窗口中间会出现自转立方体。
[group('安卓')]
android-build-3d:
    nix-shell Android.nix --run 'FEATURES=bevy-3d CARGO_TARGET_DIR=target-android cargo xtask android'

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

# 桌面 app + 内嵌 MCP server,供 AI 助手挂接(.mcp.json 里的 slint-app)。
# AI 由此能读运行中界面的元素树、截图、模拟点击,而不是靠猜。
#
# SLINT_EMIT_DEBUG_INFO=1 不能省:它让 slint 编译器把元素类型名/id 嵌进产物。少了它
# 一切照常启动,只是 get_element_tree 永远只回一个空壳根节点 —— 静默地瞎。
# 加 3D 页:just mcp-desktop-3d
[group('mcp')]
mcp-desktop: mcp-port-free
    SLINT_EMIT_DEBUG_INFO=1 SLINT_MCP_PORT={{mcp_port}} cargo run -p app-desktop --features mcp

# 同上但带 bevy 3D 页(热调面板那页才是最值得让 AI 看的)。需要 vulkan,故走 render3d.nix
[group('mcp')]
mcp-desktop-3d: mcp-port-free
    nix-shell render3d.nix --run 'SLINT_EMIT_DEBUG_INFO=1 SLINT_MCP_PORT={{mcp_port}} cargo run -p app-desktop --features mcp,bevy-3d'

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

# 起一个干净的桌面实例(带 3D),截下**真实窗口像素**,存到 dist/shot.png。
#
# width 给逻辑像素宽度即可切版式:`just shot 420` 看紧凑版(底部导航),`just shot` 看宽版。
# tab 指定开局页(0=Home、1=Server、2=3D):`just shot 420 2` 直接截紧凑版的 3D 页 ——
# 不必再靠 MCP 模拟点击切页。
#
# 为什么必须有这条 recipe:
# 1. 不先杀干净,旧实例的窗口还在,AI 截到的是**上一版**的界面,浑然不觉;
# 2. MCP 的 take_screenshot 走软件渲染器,**采不到 wgpu 纹理** —— 3D 页在它眼里恒为纯黑,
#    据此判断「3D 没渲染出来」是错的。真实像素只能靠合成器抓(niri screenshot-window)。
[group('桌面')]
shot width="" tab="0":
    #!/usr/bin/env bash
    set -euo pipefail
    just desktop-kill
    # 先编译再启动:否则「等窗口」的循环会把几分钟的编译时间也等进去,看着像卡死。
    nix-shell render3d.nix --run 'cargo build -p app-desktop --features bevy-3d'
    SLINT_STUDY_TAB={{tab}} nix-shell render3d.nix --run 'setsid target/debug/slint-study-desktop' > /tmp/slint-shot.log 2>&1 &
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
