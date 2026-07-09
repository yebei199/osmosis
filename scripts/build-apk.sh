#!/usr/bin/env bash
# 构建 Slint Study 的 debug APK。在 slint-study-builder 容器内运行
# (参见 ../build.sh),仓库挂载在 /work。
#
# 环境变量:
#   ABIS        空格分隔的 Android ABI 列表(默认 "arm64-v8a";
#               也支持 armeabi-v7a x86_64)
#   CHOWN_UID/CHOWN_GID  把产物的所有权交还给这个宿主机用户

set -euo pipefail
cd /work

ABIS="${ABIS:-arm64-v8a}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/work/.docker-target}"
JNILIBS=android/app/src/main/jniLibs

# 无论 APK 是哪个变体,native 库一律用 release profile 构建:debug
# profile 的 Slint+Skia 构建体积巨大且很慢,所以即使是"debug" APK
# 里打包的也是 release profile 的 .so 文件。
echo "==> Building Rust native libs (release profile) for ABIs: $ABIS"
rm -rf "$JNILIBS"

# Slint 的 android 后端在构建时会编译一个小的 Java helper 并以 dex
# 形式内嵌;这里让它针对已安装的 platform jar 编译(类似 Gradle 的
# compileSdk),而不是用 cargo-ndk 导出的 ANDROID_PLATFORM。
ANDROID_JAR="$(ls -d "$ANDROID_HOME"/platforms/android-*/android.jar 2>/dev/null | sort -V | tail -1)"
if [ -n "$ANDROID_JAR" ]; then
    export ANDROID_JAR
    echo "    using ANDROID_JAR=$ANDROID_JAR"
fi

TARGET_FLAGS=()
for abi in $ABIS; do TARGET_FLAGS+=(-t "$abi"); done
cargo ndk "${TARGET_FLAGS[@]}" --platform 26 -o "$JNILIBS" \
    build --lib --release --no-default-features --features android

# 打包 libc++_shared.so:Skia(Slint 的 Android 渲染器)链接的是
# 共享版 C++ STL。
for abi in $ABIS; do
    case "$abi" in
        arm64-v8a)   triple=aarch64-linux-android ;;
        armeabi-v7a) triple=arm-linux-androideabi ;;
        x86_64)      triple=x86_64-linux-android ;;
        *) echo "unsupported ABI: $abi" >&2; exit 1 ;;
    esac
    src="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/$triple/libc++_shared.so"
    if [ -f "$src" ] && [ ! -f "$JNILIBS/$abi/libc++_shared.so" ]; then
        cp "$src" "$JNILIBS/$abi/"
    fi
done

echo "==> Building debug APK"
ABIS_CSV="${ABIS// /,}"
(cd android && gradle --no-daemon -PstudyAbis="$ABIS_CSV" assembleDebug)

mkdir -p dist
OUT="dist/slint-study-debug.apk"
cp "android/app/build/outputs/apk/debug/app-debug.apk" "$OUT"

# 通过 bind mount 的构建以 root 身份运行;把产物的所有权交还给宿主机用户。
if [ -n "${CHOWN_UID:-}" ]; then
    chown -R "${CHOWN_UID}:${CHOWN_GID:-$CHOWN_UID}" \
        dist "$JNILIBS" android/app/build android/.gradle 2>/dev/null || true
fi

echo "==> Done: $OUT"
ls -lh "$OUT"
