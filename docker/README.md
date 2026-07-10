# docker/ —— 给「没有 nix」的机器准备的 APK 构建

这里是 **Docker 版**的 Android APK 构建工作流。它存在的唯一理由是**可移植**:
宿主机不需要装 nix、也不需要手工配 Android SDK/NDK,**只要有 Docker(或 Podman)**
就能出一个可复现的 APK。适合别人的机器、CI,或任何不方便用 nix 的环境。

> 如果你在 **NixOS 本机**,不必用这套——直接 `just build-apk-native` 走原生更快、
> 无镜像开销。原生工具链见仓库根的 `Android.nix`。两条路产物完全一致。

## 文件

| 文件 | 作用 |
|------|------|
| `Dockerfile` | 构建器镜像:JDK 17、Android SDK(platform 34)、NDK r27、Gradle 8、带 Android target 的 Rust、cargo-ndk |
| `build.sh` | 宿主机入口:建镜像、透传代理、挂载缓存卷、跑容器、把产物属主交回你 |

真正的编译逻辑不在这里,而在 `../xtask/`——`cargo xtask android` **容器和本机通用**,
Docker 路径和 `Android.nix` 原生路径都调它,只是工具链来源不同。

## 用法

```sh
just build-apk            # = ./docker/build.sh,产物 -> dist/slint-study-debug.apk
# 或直接:
./docker/build.sh         # 构建 debug APK
./docker/build.sh image   # 只(重新)构建 builder 镜像
./docker/build.sh shell   # 进容器交互式 shell
./docker/build.sh clean   # 清理构建产物

ABIS="x86_64" ./docker/build.sh                       # 模拟器
ABIS="arm64-v8a armeabi-v7a x86_64" ./docker/build.sh # 多 ABI
```

代理:检测到 `HTTPS_PROXY`/`http_proxy` 会自动用 `--network=host` 透传进构建
(国内访问 dl.google.com / crates.io / gradle 时是刚需)。

完整的编译流程说明见 `../docs/build-apk.md`。
