#!/usr/bin/env bash
# 真正的 APK 编译逻辑,容器/本机通用:
#   - Docker 路径:在 slint-study-builder 容器内跑,仓库挂在 /work(见 ../docker/build.sh)
#   - NixOS 原生路径:nix-shell Android.nix 里跑(见 ../Android.nix / `just build-apk-native`)
# 两者都靠 ANDROID_HOME / ANDROID_NDK_HOME 找工具链,脚本本身不假设自己在哪。
#
# 环境变量:
#   ABIS               空格分隔的 Android ABI 列表(默认 "arm64-v8a";也支持 armeabi-v7a x86_64)
#   CARGO_TARGET_DIR   Rust target 目录(默认仓库内 target-android)
#   CHOWN_UID/CHOWN_GID 仅 Docker 用:把产物所有权交还给宿主机用户(本机不设即跳过)

set -euo pipefail
# 以仓库根为工作目录,无论从哪调用(容器里 /work,本机是仓库路径)。
cd "$(dirname "$0")/.."

ABIS="${ABIS:-arm64-v8a}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target-android}"
GRADLE_PROJECT=apps/android/gradle
JNILIBS="$GRADLE_PROJECT/app/src/main/jniLibs"

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
# `-p app-android` 是必须的:workspace 的 default-members 不含它(见 docs/adr/0003),
# 而它的 Cargo.toml 已经静态选好了 android-activity 后端,无需再传 feature。
cargo ndk "${TARGET_FLAGS[@]}" --platform 26 -o "$JNILIBS" \
    build -p app-android --lib --release

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
(cd "$GRADLE_PROJECT" && gradle --no-daemon -PstudyAbis="$ABIS_CSV" assembleDebug)

mkdir -p dist
OUT="dist/slint-study-debug.apk"
cp "$GRADLE_PROJECT/app/build/outputs/apk/debug/app-debug.apk" "$OUT"

# 通过 bind mount 的构建以 root 身份运行;把产物的所有权交还给宿主机用户。
if [ -n "${CHOWN_UID:-}" ]; then
    chown -R "${CHOWN_UID}:${CHOWN_GID:-$CHOWN_UID}" \
        dist "$JNILIBS" "$GRADLE_PROJECT/app/build" "$GRADLE_PROJECT/.gradle" 2>/dev/null || true
fi

echo "==> Done: $OUT"
ls -lh "$OUT"
