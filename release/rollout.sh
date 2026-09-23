#!/usr/bin/env bash
# 把一个已发布的 release 推到所有设备(#128)。用法与资产约定见同目录 README.md。
#
#   release/rollout.sh [v0.1.15]      # 不给 tag 就取 GitHub 上最新的 release
#
# 四步各自独立:某一步失败只记进结尾的汇总,不拦后面的步骤;只有 APK 没拿到时才跳过装机。
# 有任何一步失败,退出码非零。设备离线不算失败,只在汇总里列出。
set -uo pipefail

: "${GH_REPO:=yebei199/osmosis}"
: "${ROLLOUT_BUILD_HOST:=pc3}"
: "${ROLLOUT_MODELS:?要装的设备型号(ro.product.model),空格分隔}"
: "${RELEASE_CERT_SHA256:?release APK 签名证书的 SHA-256}"
: "${OSMOSIS_API_BASE:?release 构建烘进去的服务端地址}"
: "${NIXOS_CONFIG:=$HOME/nixos_config}"
: "${ROLLOUT_DIR:=dist/rollout}"
: "${ROLLOUT_ADB_HOSTS:=}"
export GH_REPO

here=$(cd "$(dirname "$0")" && pwd)
package=io.github.osmosis
image=ghcr.io/yebei199/osmosis-server
nix_file=home/features/desktop/osmosis.nix
desktop_assets=(osmosis-desktop-x86_64-linux io.github.osmosis.desktop io.github.osmosis.svg)

tag=${1:-$(gh release view --json tagName -q .tagName)}
ver=${tag#v}
apk_name=osmosis-android-arm64-$ver.apk
apk=$ROLLOUT_DIR/$apk_name
mkdir -p "$ROLLOUT_DIR"

summary=()
failed=0
ok() { summary+=("ok    $1"); }
fail() { summary+=("FAIL  $1"); failed=1; echo "!! $1" >&2; }
skip() { summary+=("skip  $1"); }

# ---- APK ----------------------------------------------------------------

cert_ok() {
    local got
    got=$(nix shell nixpkgs#apksigner -c apksigner verify --print-certs "$1" \
        | sed -n 's/^Signer #1 certificate SHA-256 digest: //p')
    [ "$got" = "$RELEASE_CERT_SHA256" ] && return 0
    echo "签名证书 ${got:-<读不出>} 不是约定的 $RELEASE_CERT_SHA256" >&2
    return 1
}

fetch_published_apk() {
    gh release download "$tag" -p "$apk_name" -p "$apk_name.sha256" -D "$ROLLOUT_DIR" --clobber \
        && (cd "$ROLLOUT_DIR" && sha256sum -c --quiet "$apk_name.sha256")
}

build_and_upload_apk() {
    local remote_apk
    echo "==> 在 $ROLLOUT_BUILD_HOST 上构建 $tag 的 APK(冷编一小时量级)"
    remote_apk=$(ssh "$ROLLOUT_BUILD_HOST" bash -s -- "$tag" "$OSMOSIS_API_BASE" \
        < "$here/remote-build.sh" | tail -1) || return 1
    scp "$ROLLOUT_BUILD_HOST:${remote_apk:-dist/osmosis-debug.apk}" "$apk" || return 1
    cert_ok "$apk" || return 1
    (cd "$ROLLOUT_DIR" && sha256sum "$apk_name" > "$apk_name.sha256")
    gh release upload "$tag" "$apk" "$apk.sha256"
}

step_apk() {
    if gh release view "$tag" --json assets -q '.assets[].name' | grep -qxF "$apk_name"; then
        fetch_published_apk && cert_ok "$apk" && { ok "APK: 沿用 Release 上的 $apk_name"; return 0; }
        fail "APK: Release 上的 $apk_name 下载或校验失败"
        return 1
    fi
    build_and_upload_apk && { ok "APK: $ROLLOUT_BUILD_HOST 构建并上传 $apk_name"; return 0; }
    fail "APK: 构建或上传失败(签名不对时不会上传)"
    return 1
}

# ---- 设备 ---------------------------------------------------------------

online_serials() { adb devices | awk 'NR > 1 && $2 == "device" { print $1 }'; }

# 无线 adb 的连接端口会变。给了主机地址而它不在线,就扫端口逐个 connect。
reconnect_hosts() {
    local host port online
    online=$(online_serials)
    for host in $ROLLOUT_ADB_HOSTS; do
        grep -q "^$host:" <<< "$online" && continue
        echo "==> $host 不在线,扫 adb 端口"
        for port in $(nmap -p 5555,30000-49999 --open -oG - "$host" \
            | grep -oE '[0-9]+/open' | cut -d/ -f1); do
            adb connect "$host:$port" > /dev/null
        done
    done
}

install_on() {
    local serial=$1 model=$2 path remote want
    adb -s "$serial" install -r "$apk" || { fail "设备 $model($serial): adb install -r 失败"; return; }
    path=$(adb -s "$serial" shell pm path "$package" | tr -d '\r' | sed -n 's/^package://p' | head -1)
    remote=$(adb -s "$serial" shell sha256sum "$path" | cut -d' ' -f1)
    want=$(cut -d' ' -f1 "$apk.sha256")
    if [ "$remote" != "$want" ]; then
        fail "设备 $model($serial): 装上的 APK sha256 是 ${remote:-<读不出>},不是 $want"
        return
    fi
    ok "设备 $model($serial): 已装 $ver($(adb -s "$serial" shell dumpsys package "$package" \
        | grep -o 'versionName=[^ ]*' | head -1 | tr -d '\r'))"
}

step_devices() {
    local serial model seen=" "
    reconnect_hosts
    for serial in $(online_serials); do
        model=$(adb -s "$serial" shell getprop ro.product.model | tr -d '\r')
        grep -qw -- "$model" <<< "$ROLLOUT_MODELS" || continue
        seen+="$model "
        install_on "$serial" "$model"
    done
    for model in $ROLLOUT_MODELS; do
        [[ $seen == *" $model "* ]] || skip "设备 $model: 未在线,跳过(adb connect <IP>:<端口> 后重跑)"
    done
}

# ---- nixos_config -------------------------------------------------------

# 把 url 以 /<资产名>" 结尾的那一行之后的第一行 hash 换成新值。
replace_hash() {
    local asset=$1 sri=$2 file=$3
    awk -v asset="/$asset\"" -v sri="$sri" '
        index($0, asset) { pending = 1 }
        pending && /hash = "/ { sub(/hash = "[^"]*"/, "hash = \"" sri "\""); pending = 0 }
        { print }' "$file" > "$file.new" && mv "$file.new" "$file"
}

step_nixos() {
    local sums asset hex sri got url
    if [ -n "$(git -C "$NIXOS_CONFIG" status --porcelain)" ]; then
        fail "nixos_config: 工作树不干净,没动它(别的会话可能在写)"
        return
    fi
    git -C "$NIXOS_CONFIG" pull -q --ff-only || { fail "nixos_config: pull --ff-only 失败"; return; }
    gh release download "$tag" -p sha256sums.txt -D "$ROLLOUT_DIR" --clobber \
        || { fail "nixos_config: 拿不到 $tag 的 sha256sums.txt"; return; }
    sums=$ROLLOUT_DIR/sha256sums.txt

    # 先把三个哈希都核对完再落笔,半路失败就一个字都不改。
    local -A sri_of
    for asset in "${desktop_assets[@]}"; do
        hex=$(awk -v a="$asset" '$2 == a { print $1 }' "$sums")
        [ -n "$hex" ] || { fail "nixos_config: sha256sums.txt 里没有 $asset"; return; }
        sri=$(nix hash convert --hash-algo sha256 --to sri "$hex")
        url=https://github.com/$GH_REPO/releases/download/$tag/$asset
        got=$(nix store prefetch-file --json --hash-type sha256 "$url" | jq -r .hash)
        [ "$got" = "$sri" ] || { fail "nixos_config: $asset 实取 $got 与 sha256sums.txt 的 $sri 不符"; return; }
        sri_of[$asset]=$sri
    done

    local file=$NIXOS_CONFIG/$nix_file
    sed -i -E "0,/version = \"[^\"]*\";/s//version = \"$ver\";/" "$file"
    for asset in "${desktop_assets[@]}"; do replace_hash "$asset" "${sri_of[$asset]}" "$file"; done

    if git -C "$NIXOS_CONFIG" diff --quiet; then
        ok "nixos_config: 已是 $ver,无需提交"
        return
    fi
    if git -C "$NIXOS_CONFIG" commit -q -am "chore(osmosis): bump desktop release to $ver" \
        && git -C "$NIXOS_CONFIG" push -q; then
        ok "nixos_config: 已抬到 $ver 并推送($(git -C "$NIXOS_CONFIG" rev-parse --short HEAD))"
    else
        fail "nixos_config: 提交或推送失败"
    fi
}

# ---- 服务端镜像 ---------------------------------------------------------

step_server() {
    local manifest digest
    manifest=$(docker buildx imagetools inspect --format '{{json .Manifest}}' "$image:$ver") \
        || { fail "镜像: 查不到 $image:$ver"; return; }
    digest=$(jq -r .digest <<< "$manifest")
    if ! jq -e '[.manifests[].platform.architecture] | index("arm64")' <<< "$manifest" > /dev/null; then
        fail "镜像: $image:$ver 不含 arm64"
        return
    fi
    ok "镜像(arm64 已核):$image:$ver@$digest —— 交给 infra 会话去钉"
}

# ---- 主流程 -------------------------------------------------------------

echo "==> 推送 $tag"
if step_apk; then step_devices; else skip "设备: 没有可装的 APK"; fi
step_nixos
step_server

echo
echo "==> $tag 汇总"
printf '    %s\n' "${summary[@]}"
echo "==> 桌面端请自行 nixos-rebuild。"
exit $failed
