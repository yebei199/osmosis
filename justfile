apk := "dist/slint-study-debug.apk"

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
# 可透传额外 feature,如左上角帧率读数:just dev debug-fps
[group('三端')]
[group('桌面')]
dev extra="":
    SLINT_LIVE_PREVIEW=1 cargo run -p app-desktop --features slint/live-preview{{ if extra != "" { "," + extra } else { "" } }}

# 桌面 + 嵌入的 bevy 3D 面板(见 crates/render3d)。走 render3d.nix 拿 vulkan 运行期库。
# 无热重载:bevy 场景在 Rust 里,改完重跑。首帧就绪后窗口中间会出现一个自转的立方体。
[group('桌面')]
dev-3d:
    nix-shell render3d.nix --run 'cargo run -p app-desktop --features bevy-3d'

# 网页版:编译 wasm + 生成胶水代码 + 起静态服务器,浏览器开 http://127.0.0.1:8080
# 本命令自带 dev-server,不必另开终端 —— 「Check server」开箱即通。
# 无热重载(浏览器加载的是打包产物),改完代码重跑本命令并刷新页面。
# 用 release:debug 的 wasm 有上百 MB,浏览器加载能等到天荒地老。
[group('三端')]
dev-web:
    nix-shell slint.nix --run 'cargo build -p app-web --target wasm32-unknown-unknown --release'
    nix-shell slint.nix --run 'wasm-bindgen target/wasm32-unknown-unknown/release/app_web.wasm --target web --no-typescript --out-dir dist/web'
    cp apps/web/index.html dist/web/
    # server 不在 default-members 里,裸 cargo build 从不编它。先编完再起,否则页面
    # 已经能开、按钮却要再等半分钟才通,报的还是「网络错误」,徒增困惑。
    nix-shell slint.nix --run 'cargo build -p server'
    # 上次 Ctrl-C 没杀干净的 server 还占着 8080,先收尸。只匹配本命令起的进程,不误伤别人的 8080。
    # [.] 是为了让这行自己的命令行不被这个正则匹配上 —— 否则 pkill 会连本 recipe 一起杀。
    pkill -f 'http[.]server 8080 -d dist/web' || true
    # server 放后台、http.server 占前台,Ctrl-C 走 trap 把 server 一起带走。
    # 3000 被占说明已有一个 server 在跑(比如 android 调试用的那个),这个会 panic
    # 退出、不影响前端,那边的链路也毫发无伤。不为此发明端口探测。
    nix-shell slint.nix --run 'cargo run -p server & trap "kill %1 2>/dev/null" EXIT; python3 -m http.server 8080 -d dist/web'

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
dev-server:
    cargo run -p server

# 打一次真实的 GET /health(需要 dev-server 正在另一个终端里跑)
[group('服务端')]
test-api:
    cargo test -p api -- --ignored

# NixOS 本机原生编译 dist APK(更快、无镜像开销)。前提:已 `rustup default stable`
# ABIS 可选:ABIS="x86_64" just build-apk-native
[group('三端')]
[group('安卓')]
build-apk-native:
    nix-shell Android.nix --run 'CARGO_TARGET_DIR=target-android cargo xtask android'

# 同上,但把 bevy 3D 面板(见 crates/render3d)编进 APK。native 库仍是 release
# profile(bevy debug 产物几百 MB)。装机后窗口中间会出现自转立方体。
[group('安卓')]
build-apk-3d:
    nix-shell Android.nix --run 'FEATURES=bevy-3d CARGO_TARGET_DIR=target-android cargo xtask android'

# USB 直装到手机(推荐:不受移动热点/公司 WiFi 客户端隔离影响)
[group('安卓')]
install-apk:
    adb install -r {{apk}}

# 把手机的 127.0.0.1:3000 转发到开发机的 dev-server
# 手机上的 127.0.0.1 指的是手机自己,不转发的话「Check server」永远失败。
# adb 重连后需要重新执行
[group('安卓')]
adb-reverse:
    adb reverse tcp:3000 tcp:3000

# 装 APK、接通端口转发,然后看日志。前提:dev-server 已在另一个终端里跑
[group('安卓')]
run-android: install-apk adb-reverse
    adb shell am start -n io.github.slintstudy/.MainActivity
    adb logcat -s slint_study

# 局域网 http 共享,手机扫码下载
# 可用前提:手机与电脑同一网络且无客户端隔离(如电脑自己开的热点)
# 连的是别人的移动热点/公司 WiFi 多半被隔离,手机连不上,请改用 install-apk
[group('安卓')]
serve-apk:
    miniserve dist --interfaces 0.0.0.0 --port 3070 --qrcode
