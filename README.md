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
docker/            Docker 构建工作流(给没有 nix 的机器);见 docker/README.md
docker/build.sh    Docker 宿主机入口
docker/Dockerfile  构建器镜像(JDK、Android SDK+NDK、Rust、cargo-ndk)
Android.nix        NixOS 本机原生工具链(nix-shell)
scripts/build-apk.sh   真正的编译逻辑,容器/本机通用(cargo-ndk + gradle assembleDebug)
docs/build-apk.md  APK 构建全流程与编译逻辑详解
```

## 为什么有 Java 代码?

UI 是 100% Slint,逻辑是 100% Rust。`android/` 目录下唯一的 Java 文件
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
just build-apk         # Docker:给没有 nix 的机器/CI(= ./docker/build.sh)
just build-apk-native  # NixOS 本机原生:更快、无镜像开销(nix-shell Android.nix)
adb install -r dist/slint-study-debug.apk
```

为模拟器(x86_64)或多个 ABI 构建:

```sh
ABIS="x86_64" just build-apk               # 或 just build-apk-native
ABIS="arm64-v8a armeabi-v7a x86_64" just build-apk
```

- Docker 路径细节见 [`docker/README.md`](docker/README.md);
- 完整编译流程见 [`docs/build-apk.md`](docs/build-apk.md)。

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
