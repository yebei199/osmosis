#!/usr/bin/env bash
# Slint Study build entrypoint — everything runs in a Docker container, so the
# only host requirement is Docker (or Podman).
#
# Usage:
#   ./build.sh            build the debug APK -> dist/slint-study-debug.apk
#   ./build.sh image      (re)build the builder image only
#   ./build.sh shell      interactive shell in the builder container
#   ./build.sh clean      remove build artifacts
#
# Environment:
#   ABIS="arm64-v8a armeabi-v7a x86_64"   ABIs to build (default arm64-v8a;
#                                         use x86_64 for the emulator)
#   SKIP_IMAGE_BUILD=1                    reuse an existing builder image
#   DOCKER=podman                         force a specific container tool

set -euo pipefail
cd "$(dirname "$0")"

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

# Behind a proxy (common in mainland China: dl.google.com / crates.io / gradle
# are otherwise unreachable), forward the host proxy into the build. --network=host
# lets the container reach a 127.0.0.1 proxy; curl/rustup/cargo read the predefined
# http_proxy args, and JVM tools (sdkmanager, gradle) need -Dhttp(s).proxyHost.
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
    # Named volumes cache the cargo registry and gradle artifacts across builds;
    # the repo bind mount carries the rust target dir (.docker-target).
    "$DOCKER" run --rm "${DOCKER_RUN_EXTRA[@]}" \
        -v "$PWD:/work:z" \
        -v slint-study-cargo-registry:/opt/cargo/registry \
        -v slint-study-gradle-home:/root/.gradle \
        -e ABIS="${ABIS:-arm64-v8a}" \
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
        run_in_container scripts/build-apk.sh
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
            "rm -rf /work/.docker-target /work/dist /work/android/app/build /work/android/build /work/android/.gradle /work/android/app/src/main/jniLibs"
        ;;
    *)
        echo "unknown command: $1 (expected: apk | image | shell | clean)" >&2
        exit 1
        ;;
esac
