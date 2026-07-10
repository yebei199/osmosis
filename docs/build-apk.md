# APK 构建逻辑

## 两条路,一个内核

真正的编译逻辑只有一份:`scripts/build-apk.sh`(cargo-ndk 交叉编译 → gradle 打包)。
它**容器和本机通用**,靠 `ANDROID_HOME` / `ANDROID_NDK_HOME` 找工具链,不假设自己
在哪。外面套两种「怎么把工具链准备好」的方式:

```
                                        ┌─ docker/build.sh ─→ Docker 容器 ─┐
你 → just build-apk / build-apk-native ─┤                                  ├─→ scripts/build-apk.sh(真正编译)
                                        └─ nix-shell Android.nix ──────────┘
```

| 路径 | 命令 | 工具链来源 | 适合 |
|------|------|-----------|------|
| Docker | `just build-apk` | `docker/Dockerfile` 镜像 | 没有 nix 的机器 / CI,只需 Docker |
| NixOS 原生 | `just build-apk-native` | `Android.nix`(nix-shell) | 你在 NixOS 本机,更快、无镜像开销 |

两者产物完全一致。下面先讲 Docker 那层「外围事务」,再讲两条路共用的编译内核。

## 为什么要套一层 Docker

一个能出 APK 的机器要同时具备:JDK 17、Android SDK(platform 34 + build-tools)、
NDK r27、Gradle 8.11、带 Android target 的 Rust、cargo-ndk。手工在每台机器上装齐
既慢又容易版本漂移。`docker/Dockerfile` 把这些固定成一个可复现的镜像
`slint-study-builder`,于是构建结果不再依赖「你这台机器装了什么」。(NixOS 上则由
`Android.nix` 从 nix 二进制缓存提供同样版本的工具链,见文末「NixOS 原生路径」。)

`docker/build.sh` 负责的就是这层「外围事务」,它自己不碰编译:

- **选容器工具**:优先 `docker`,没有就用 `podman`。
- **透传代理**:检测到 `HTTPS_PROXY`/`http_proxy` 时,用 `--network=host` 把宿主机
  代理转发进构建;并额外拼出 `-Dhttp(s).proxyHost=...`(JVM 工具 sdkmanager/gradle
  只认这套参数,不认环境变量)。国内访问 dl.google.com / crates.io / gradle 时是刚需。
- **挂载卷**:仓库挂到 `/work`;cargo registry 和 gradle home 用具名卷跨构建缓存;
  Rust 的 `target` 落在仓库里的 `.docker-target`。
- **分发子命令**:`apk`(默认)/ `image` / `shell` / `clean`。
- **交回属主**:容器内以 root 构建,产物用 `CHOWN_UID/GID` 改回当前宿主机用户。

真正的编译逻辑全部在容器内的 `scripts/build-apk.sh`。

## `scripts/build-apk.sh` 的三步编译

### 1. 用 cargo-ndk 交叉编译 Rust native 库(release profile)

```
cargo ndk -t <abi> --platform 26 -o apps/android/gradle/app/src/main/jniLibs \
    build -p app-android --lib --release
```

- 编的是 `[lib] crate-type = ["cdylib"]`,产物是 `libslint_study.so`——APK 里被
  `NativeActivity` 加载的那个 `.so`。
- **即使是「debug」APK,native 库也一律用 `--release`**:debug profile 的
  Slint + Skia 体积巨大且极慢,所以打包进去的始终是 release profile 的 `.so`。
- `-p app-android` 指定平台入口 crate;它的 Cargo.toml 静态选定了 `slint/backend-android-activity-06`(Android 后端 +
  Skia 渲染器);`--no-default-features` 关掉桌面用的 winit/femtovg。
- 输出直接落进 Gradle 会打包的 `jniLibs/<abi>/`。
- 额外从 NDK sysroot 拷一份 `libc++_shared.so`:Skia 链接的是共享版 C++ STL。

> 顺带一提:`build.rs` 在这一步里由 Cargo 触发,用 `slint-build` 把
> `crates/ui/slint/app.slint` 编译成 Rust 代码(material 风格),和 Android 无关,桌面构建也走同一条。

### 2. 用 Gradle 打包 APK

```
cd apps/android/gradle && gradle --no-daemon -PstudyAbis="$ABIS_CSV" assembleDebug
```

- 此时 Rust 已经编完,Gradle **不碰 Rust**,只负责组装 Android 壳:
  - 编译极薄的 `MainActivity.java`(仅设置 edge-to-edge 全面屏,UI 全在 Slint 里);
  - 把 `jniLibs/<abi>/*.so` 塞进 APK;
  - `-PstudyAbis` 通过 `abiFilters` 保证只打包这次真正构建了的 ABI;
  - 用 AGP 自动生成的 debug keystore 签名。
- `AndroidManifest.xml` 里 `android.app.lib_name = slint_study` 告诉 NativeActivity
  加载哪个 `.so`;android-activity 胶水再调用 Rust 里的 `android_main`。

### 3. 收尾

把 `apps/android/gradle/app/build/outputs/apk/debug/app-debug.apk` 拷成
`dist/slint-study-debug.apk`。若设了 `CHOWN_UID`(仅 Docker 传)则把产物属主交回
宿主机用户;本机原生构建不设这个变量,自然跳过。

## NixOS 原生路径

`just build-apk-native` 不用 Docker,而是 `nix-shell Android.nix --run
'CARGO_TARGET_DIR=target-android scripts/build-apk.sh'`——**编译内核是同一个脚本**,
只是工具链来自 nix 而非容器。`Android.nix` 提供和 Docker 镜像同版本的 SDK/NDK/Gradle/
cargo-ndk,并在 `shellHook` 里填了两处 NixOS 特有的坑:

- **aapt2**:AGP 默认从 Maven 拉的 `aapt2` 二进制在 NixOS 上跑不了(FHS 动态链接);
  用 `-Dorg.gradle.project.android.aapt2FromMavenOverride=...` 指向 androidenv 里
  已 patchelf 过的 aapt2。
- **rustup target**:进 shell 时自动 `rustup target add aarch64-linux-android ...`
  (前提:已 `rustup default stable` 且联网)。

产物与 Docker 完全一致。哪条更快:热构建两边几乎相同(瓶颈都是 Slint+Skia 的
release 编译);冷启动原生更快(nix 二进制缓存替换 vs Docker 现装镜像)。

## ABI 与变体

- 默认只构建 `arm64-v8a`(真机)。模拟器用 `ABIS="x86_64" just build-apk`;
  多 ABI 用 `ABIS="arm64-v8a armeabi-v7a x86_64" just build-apk`。
- 目前只有 debug 变体(`assembleDebug`)。gradle 里保留了 `release` buildType,
  但脚本未接;需要 release APK 得自己接签名配置,不是现成的。

## 相关文件一览

| 文件 | 职责 |
|------|------|
| `docker/build.sh` | Docker 宿主机入口:建镜像、透传代理、挂卷、跑容器、交回属主 |
| `docker/Dockerfile` | 可复现的工具链镜像(JDK/SDK/NDK/Gradle/Rust/cargo-ndk) |
| `Android.nix` | NixOS 本机原生工具链(nix-shell),Docker 的等价替代 |
| `scripts/build-apk.sh` | 真正的编译逻辑,容器/本机通用(cargo-ndk → gradle → dist) |
| `Cargo.toml` | `crate-type=cdylib`、`android`/`desktop` feature、release profile |
| `apps/android/gradle/app/build.gradle` | Android 打包配置、abiFilters、buildTypes |
| `apps/android/gradle/.../MainActivity.java` | 极薄 NativeActivity,只做全面屏 |
