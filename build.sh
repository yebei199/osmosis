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

build_image() {
    if [ "${SKIP_IMAGE_BUILD:-0}" = "1" ]; then
        echo "==> SKIP_IMAGE_BUILD=1: reusing existing '$IMAGE' image"
        return 0
    fi
    "$DOCKER" build -t "$IMAGE" docker/
}

run_in_container() {
    # Named volumes cache the cargo registry and gradle artifacts across builds;
    # the repo bind mount carries the rust target dir (.docker-target).
    "$DOCKER" run --rm \
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
