#!/usr/bin/env bash
# 在构建机上编一个 tag 的 release APK。rollout.sh 用 `ssh <host> bash -s -- <tag> <api_base>`
# 把本文件喂过去,不要求构建机上有本仓库的 checkout。
#
# stdout 只有最后一行:APK 的绝对路径。构建日志全走 stderr。
set -euo pipefail

tag=$1
api_base=$2
src=${OSMOSIS_BUILD_DIR:-$HOME/.cache/osmosis-release}
keystore=${RELEASE_KEYSTORE:-$HOME/.config/osmosis/release.keystore}

# 没有约定的那把 key 就不编:gradle 会退回本机的 debug keystore,签出来的包装不上用户的设备。
[ -f "$keystore" ] || { echo "缺签名密钥 $keystore,见 release/README.md「签名」" >&2; exit 1; }

{
    [ -d "$src/.git" ] || git clone https://github.com/yebei199/osmosis.git "$src"
    cd "$src"
    git fetch --tags --force origin
    git checkout -q -f --detach "$tag"

    # debug keystore 的口令与别名是 AGP 的公开默认值,不是秘密;秘密是那个文件本身。
    # 走 ORG_GRADLE_PROJECT_ 注入而不是改 gradle 配置:老 tag 也能用同一把 key 签。
    env \
        "ORG_GRADLE_PROJECT_android.injected.signing.store.file=$keystore" \
        "ORG_GRADLE_PROJECT_android.injected.signing.store.password=android" \
        "ORG_GRADLE_PROJECT_android.injected.signing.key.alias=androiddebugkey" \
        "ORG_GRADLE_PROJECT_android.injected.signing.key.password=android" \
        OSMOSIS_API_BASE="$api_base" \
        nice -n 19 nix-shell Android.nix --run 'CARGO_TARGET_DIR=target-android cargo xtask android'
} >&2

echo "$src/dist/osmosis-debug.apk"
