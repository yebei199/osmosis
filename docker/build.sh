#!/usr/bin/env bash
# Slint Study 的 Docker 构建入口 —— 一切都在 Docker 容器里运行,宿主机唯一
# 需要的就是 Docker(或 Podman)。这是给「没有 nix」的机器/CI 准备的;
# NixOS 本机原生编译请用 `just build-apk-native`(见 docker/README.md)。
#
# 用法:
#   ./docker/build.sh            构建 debug APK -> dist/slint-study-debug.apk
#   ./docker/build.sh image      只(重新)构建 builder 镜像
#   ./docker/build.sh shell      在 builder 容器内打开交互式 shell
#   ./docker/build.sh clean      清理构建产物
#
# 环境变量:
#   ABIS="arm64-v8a armeabi-v7a x86_64"   要构建的 ABI(默认 arm64-v8a;
#                                         模拟器请用 x86_64)
#   SKIP_IMAGE_BUILD=1                    复用已有的 builder 镜像
#   DOCKER=podman                         强制指定容器工具

set -euo pipefail
# 脚本位于 docker/,但一切以仓库根为准:/work 挂的是仓库根,镜像上下文是 docker/。
cd "$(dirname "$0")/.."

if [ -z "${DOCKER:-}" ]; then
    if command -v docker >/dev/null 2>&1; then
        DOCKER=docker
    elif command -v podman >/dev/null 2>&1; then
        DOCKER=podman
    else
        echo "error: docker (or podman) is required" >&2
        exit 1
    fi
fi

IMAGE=slint-study-builder

# 处于代理之后时(国内常见情况: dl.google.com / crates.io / gradle
# 否则都无法访问),把宿主机代理转发进构建过程。--network=host 让
# 容器能访问 127.0.0.1 的代理;curl/rustup/cargo 读取预置的
# http_proxy 参数,而 JVM 工具(sdkmanager、gradle)需要
# -Dhttp(s).proxyHost。
DOCKER_BUILD_EXTRA=()
DOCKER_RUN_EXTRA=()
PROXY="${HTTPS_PROXY:-${https_proxy:-${HTTP_PROXY:-${http_proxy:-}}}}"
if [ -n "$PROXY" ]; then
    echo "==> Using proxy $PROXY (host network) for the build"
    hostport="${PROXY#*://}"
    jvm_proxy="-Dhttp.proxyHost=${hostport%%:*} -Dhttp.proxyPort=${hostport##*:} -Dhttps.proxyHost=${hostport%%:*} -Dhttps.proxyPort=${hostport##*:}"
    DOCKER_BUILD_EXTRA=(--network=host
        --build-arg "http_proxy=$PROXY" --build-arg "https_proxy=$PROXY"
        --build-arg "HTTP_PROXY=$PROXY" --build-arg "HTTPS_PROXY=$PROXY"
        --build-arg "JAVA_TOOL_OPTIONS=$jvm_proxy")
    DOCKER_RUN_EXTRA=(--network=host
        -e "http_proxy=$PROXY" -e "https_proxy=$PROXY"
        -e "HTTP_PROXY=$PROXY" -e "HTTPS_PROXY=$PROXY"
        -e "JAVA_TOOL_OPTIONS=$jvm_proxy")
fi

build_image() {
    if [ "${SKIP_IMAGE_BUILD:-0}" = "1" ]; then
        echo "==> SKIP_IMAGE_BUILD=1: reusing existing '$IMAGE' image"
        return 0
    fi
    "$DOCKER" build "${DOCKER_BUILD_EXTRA[@]}" -t "$IMAGE" docker/
}

run_in_container() {
    # 具名 volume 用于跨构建缓存 cargo registry 和 gradle 产物;
    # 仓库的 bind mount 承载 rust 的 target 目录(.docker-target)。
    "$DOCKER" run --rm "${DOCKER_RUN_EXTRA[@]}" \
        -v "$PWD:/work:z" \
        -v slint-study-cargo-registry:/opt/cargo/registry \
        -v slint-study-gradle-home:/root/.gradle \
        -e ABIS="${ABIS:-arm64-v8a}" \
        -e CARGO_TARGET_DIR="/work/.docker-target" \
        -e CHOWN_UID="$(id -u)" \
        -e CHOWN_GID="$(id -g)" \
        "$IMAGE" "$@"
}

case "${1:-apk}" in
    image)
        build_image
        ;;
    apk)
        build_image
        run_in_container cargo xtask android
        echo
        echo "Install with: adb install -r dist/slint-study-debug.apk"
        ;;
    shell)
        build_image
        "$DOCKER" run --rm -it \
            -v "$PWD:/work:z" \
            -v slint-study-cargo-registry:/opt/cargo/registry \
            -v slint-study-gradle-home:/root/.gradle \
            "$IMAGE" bash
        ;;
    clean)
        build_image
        run_in_container bash -c \
            "rm -rf /work/.docker-target /work/dist /work/apps/android/gradle/app/build /work/apps/android/gradle/build /work/apps/android/gradle/.gradle /work/apps/android/gradle/app/src/main/jniLibs"
        ;;
    *)
        echo "unknown command: $1 (expected: apk | image | shell | clean)" >&2
        exit 1
        ;;
esac
