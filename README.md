# Slint Study

一个最小化的 [Slint](https://slint.dev) Android 应用 —— 一个点击计数器,构建结构
参照 `ntrack` 项目。UI 用 Slint 编写,逻辑用 Rust 编写,整体编译为一个
被 `NativeActivity` 加载的 native `.so`。APK 在 Docker 容器内可复现地构建,
宿主机唯一需要的依赖就是 Docker。

## 目录结构

```
ui/app.slint       整个 UI(一个计数器)
src/lib.rs         android_main + run_app(UI<->Rust 的胶水代码)
src/main.rs        桌面开发入口
android/           Gradle 项目:NativeActivity、manifest、资源
docker/Dockerfile  构建器镜像(JDK、Android SDK+NDK、Rust、cargo-ndk)
scripts/build-apk.sh   cargo-ndk 交叉构建 + gradle assembleDebug
build.sh           宿主机入口(驱动 Docker)
```

## 构建 APK

```sh
./build.sh                 # -> dist/slint-study-debug.apk
adb install -r dist/slint-study-debug.apk
```

为模拟器(x86_64)或多个 ABI 构建:

```sh
ABIS="x86_64" ./build.sh
ABIS="arm64-v8a armeabi-v7a x86_64" ./build.sh
```

## 桌面开发构建

在桌面上运行同一套 UI(需要 `libfontconfig1-dev`):

```sh
cargo run --features desktop
```

如果需要热重载 UI,改用 `just dev`:它启用了 Slint 的 `live-preview`
特性,编辑 `ui/*.slint` 会直接刷新运行中的窗口,无需重新编译或重启
(Rust 逻辑会保留;修改 Rust 代码仍需重启)。

```sh
just dev
```

## License

MIT
