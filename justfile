apk := "dist/slint-study-debug.apk"

# 本地跑一遍 CI 会跑的全部检查。dev 上的 push 不触发 CI,提交前跑这个
# 与 .github/workflows/ci.yml 一一对应,命令逐字相同
ci: ci-test ci-cross ci-boundaries
    @echo "==> CI 全部通过"

# 桌面链路:单测 + clippy(-D warnings,和 CI 一致)
ci-test:
    nix-shell slint.nix --run 'RUSTFLAGS="-D warnings" cargo test'
    nix-shell slint.nix --run 'RUSTFLAGS="-D warnings" cargo test -p xtask'
    nix-shell slint.nix --run 'RUSTFLAGS="-D warnings" cargo clippy --all-targets'
    nix-shell slint.nix --run 'RUSTFLAGS="-D warnings" cargo clippy --all-targets -p server -p xtask'

# 本地跑不动的端,至少保证能编译。android 的 build.rs 要 platform jar,故走 Android.nix
ci-cross:
    nix-shell slint.nix --run 'cargo check -p app-web --target wasm32-unknown-unknown'
    nix-shell slint.nix --run 'cargo check -p app-ios --target aarch64-apple-ios'
    nix-shell Android.nix --run 'cargo check -p app-android --target aarch64-linux-android'

# 架构边界(docs/adr/0001、0002)。与 CI 调的是同一份 xtask 代码
ci-boundaries:
    nix-shell slint.nix --run 'cargo xtask boundaries'

# 热重载 UI 开发:编辑 crates/ui/slint/*.slint 保存即刷新运行中的窗口(改 Rust 逻辑仍需重启)
dev:
    SLINT_LIVE_PREVIEW=1 cargo run -p app-desktop --features slint/live-preview

# 同上,外加左上角帧率读数。注意 debug-fps 会让渲染循环满转
dev-fps:
    SLINT_LIVE_PREVIEW=1 cargo run -p app-desktop --features slint/live-preview,debug-fps

# 开发服务端,监听 127.0.0.1:3000。「Check server」按钮打的就是它
dev-server:
    cargo run -p server

# 打一次真实的 GET /health(需要 dev-server 正在另一个终端里跑)
test-api:
    cargo test -p api -- --ignored


# 在 Docker 里交叉编译 dist APK —— 给没有 nix 的机器/CI 用(宿主机只需 Docker/Podman)
# ABIS 可选:默认 arm64-v8a;模拟器用 x86_64
build-apk:
    ./docker/build.sh

# NixOS 本机原生编译 dist APK(更快、无镜像开销)。前提:已 `rustup default stable`
# ABIS 可选:ABIS="x86_64" just build-apk-native
build-apk-native:
    nix-shell Android.nix --run 'CARGO_TARGET_DIR=target-android cargo xtask android'

# USB 直装到手机(推荐:不受移动热点/公司 WiFi 客户端隔离影响)
install-apk:
    adb install -r {{apk}}

# 把手机的 127.0.0.1:3000 转发到开发机的 dev-server
# 手机上的 127.0.0.1 指的是手机自己,不转发的话「Check server」永远失败。
# adb 重连后需要重新执行
adb-reverse:
    adb reverse tcp:3000 tcp:3000

# 装 APK、接通端口转发,然后看日志。前提:dev-server 已在另一个终端里跑
run-android: install-apk adb-reverse
    adb shell am start -n io.github.slintstudy/.MainActivity
    adb logcat -s slint_study

# 局域网 http 共享,手机扫码下载
# 可用前提:手机与电脑同一网络且无客户端隔离(如电脑自己开的热点)
# 连的是别人的移动热点/公司 WiFi 多半被隔离,手机连不上,请改用 install-apk
serve-apk:
    miniserve dist --interfaces 0.0.0.0 --port 3070 --qrcode
