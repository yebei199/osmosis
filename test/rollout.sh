#!/usr/bin/env bash
# release/rollout.sh 的回归检查(#128)。把假的 gh / ssh / scp / adb / nix / docker / nmap
# 顶在 PATH 前面,nixos_config 用一个临时 git 仓库加本地裸仓库当远端。不连网、不碰真
# 设备、不编译,几秒跑完。任何一条断言不成立就非零退出。
#
# 覆盖:Release 上已有 APK 就沿用、没有就远程构建并上传两份资产;签名不对拒绝上传与安装;
# 按型号挑设备、名单外的不装、离线的汇总;装完校验设备上的 APK 哈希;nixos_config 抬版本
# 与哈希(prefetch 核对)并推送、工作树不干净就不动、prefetch 对不上就不动;镜像引用与
# arm64 核对。
set -euo pipefail

cd "$(dirname "$0")/.."
root=$PWD
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

good_cert=030e9bc786079d54952b0f7f4cafacb6af6417776db69b2378921ca641717e72
mkdir "$work/bin"

cat > "$work/bin/gh" <<'EOF'
#!/usr/bin/env bash
echo "gh $*" >> "$FAKE_CALLS"
case "$1 $2" in
    "release view")
        if [ "$3" = --json ]; then echo v0.1.15; exit 0; fi
        ls "$FAKE_REL" ;;
    "release download")
        shift 3; dir=.; pats=()
        while [ $# -gt 0 ]; do
            case $1 in -p) pats+=("$2"); shift 2 ;; -D) dir=$2; shift 2 ;; *) shift ;; esac
        done
        for p in "${pats[@]}"; do cp "$FAKE_REL/$p" "$dir/" || exit 1; done ;;
    "release upload")
        shift 3
        for f in "$@"; do [ "${f#-}" = "$f" ] && cp "$f" "$FAKE_REL/"; done ;;
esac
exit 0
EOF

cat > "$work/bin/ssh" <<'EOF'
#!/usr/bin/env bash
echo "ssh $*" >> "$FAKE_CALLS"
cat > /dev/null
EOF

cat > "$work/bin/scp" <<'EOF'
#!/usr/bin/env bash
echo "scp $*" >> "$FAKE_CALLS"
cp "$FAKE_BUILT_APK" "${@: -1}"
EOF

# 假 nix:apksigner 报 FAKE_CERT;hash convert 把十六进制写成 "sha256-<hex>";
# prefetch-file 取 Release 目录里同名文件的真实 sha256,同样写成 "sha256-<hex>"。
cat > "$work/bin/nix" <<'EOF'
#!/usr/bin/env bash
case "$1" in
    shell) echo "Signer #1 certificate SHA-256 digest: $FAKE_CERT" ;;
    hash) echo "sha256-${@: -1}" ;;
    store)
        f="$FAKE_REL/$(basename "${@: -1}")"
        [ -f "$f" ] || exit 1
        printf '{"hash":"sha256-%s"}\n' "$(sha256sum "$f" | cut -d' ' -f1)" ;;
esac
EOF

# 假 adb:在线设备取 FAKE_DEVICES("序列号=型号"空格分隔)。装包时把文件记下来,
# 之后 sha256sum 读回它;FAKE_CORRUPT_INSTALL 置位时读回一个错的哈希。
cat > "$work/bin/adb" <<'EOF'
#!/usr/bin/env bash
echo "adb $*" >> "$FAKE_CALLS"
if [ "$1" = devices ]; then
    echo "List of devices attached"
    for d in $FAKE_DEVICES; do printf '%s\tdevice\n' "${d%%=*}"; done
    exit 0
fi
[ "$1" = -s ] || exit 0
serial=$2; shift 2
model=
for d in $FAKE_DEVICES; do [ "${d%%=*}" = "$serial" ] && model=${d#*=}; done
case "$*" in
    "shell getprop ro.product.model") printf '%s\r\n' "$model" ;;
    "install -r "*) cp "$3" "$FAKE_STATE/$serial.apk" ;;
    "shell pm path io.github.osmosis") printf 'package:/data/app/x/base.apk\r\n' ;;
    "shell sha256sum "*)
        if [ -n "${FAKE_CORRUPT_INSTALL:-}" ]; then echo "0000  x"
        else sha256sum "$FAKE_STATE/$serial.apk"; fi ;;
    "shell dumpsys package io.github.osmosis") echo "    versionName=0.1.15" ;;
esac
EOF

cat > "$work/bin/docker" <<'EOF'
#!/usr/bin/env bash
echo "docker $*" >> "$FAKE_CALLS"
echo "$FAKE_MANIFEST"
EOF
printf '#!/bin/sh\necho "nmap $*" >> "$FAKE_CALLS"\n' > "$work/bin/nmap"
chmod +x "$work/bin/"*

export PATH="$work/bin:$PATH" FAKE_CALLS="$work/calls" FAKE_CERT=$good_cert \
    RELEASE_CERT_SHA256=$good_cert GH_REPO=yebei199/osmosis ROLLOUT_BUILD_HOST=pc3 \
    ROLLOUT_MODELS="NP06J SM-S9180" OSMOSIS_API_BASE=https://example.invalid \
    FAKE_MANIFEST='{"digest":"sha256:abc","manifests":[{"platform":{"architecture":"arm64"}}]}'
unset ROLLOUT_ADB_HOSTS

# 每个用例一套新的 Release 目录、设备状态和 nixos_config。
setup() {
    rm -rf "$work/case" && mkdir -p "$work/case/rel" "$work/case/state" "$work/case/out"
    export FAKE_REL="$work/case/rel" FAKE_STATE="$work/case/state" ROLLOUT_DIR="$work/case/out"
    export FAKE_BUILT_APK="$work/case/built.apk" FAKE_DEVICES="dev=NP06J mi=2211133C"
    unset FAKE_CORRUPT_INSTALL
    echo "apk-bytes" > "$FAKE_BUILT_APK"
    for f in osmosis-desktop-x86_64-linux io.github.osmosis.desktop io.github.osmosis.svg; do
        echo "$f-bytes" > "$FAKE_REL/$f"
    done
    (cd "$FAKE_REL" && sha256sum osmosis-desktop-x86_64-linux io.github.osmosis.desktop \
        io.github.osmosis.svg > sha256sums.txt)

    git init -q --bare "$work/case/origin.git"
    git init -q -b master "$work/case/nixos"
    mkdir -p "$work/case/nixos/home/features/desktop"
    cat > "$work/case/nixos/home/features/desktop/osmosis.nix" <<'NIX'
    version = "0.1.14";
      url = "https://github.com/yebei199/osmosis/releases/download/v${finalAttrs.version}/osmosis-desktop-x86_64-linux";
      hash = "sha256-old1";
      url = "https://github.com/yebei199/osmosis/releases/download/v${finalAttrs.version}/io.github.osmosis.desktop";
      hash = "sha256-old2";
      url = "https://github.com/yebei199/osmosis/releases/download/v${finalAttrs.version}/io.github.osmosis.svg";
      hash = "sha256-old3";
NIX
    git -C "$work/case/nixos" config user.name t
    git -C "$work/case/nixos" config user.email t@t
    git -C "$work/case/nixos" add -A
    git -C "$work/case/nixos" commit -qm init
    git -C "$work/case/nixos" remote add origin "$work/case/origin.git"
    git -C "$work/case/nixos" push -q -u origin master
    export NIXOS_CONFIG="$work/case/nixos"
}

fails=0
run() {   # run <描述> <0 = 期望成功 | 1 = 期望失败> [参数...]
    local name=$1 want=$2; shift 2
    : > "$FAKE_CALLS"
    set +e; "$root/release/rollout.sh" "$@" > "$work/out" 2>&1; local got=$?; set -e
    if { [ "$want" = 0 ] && [ $got -eq 0 ]; } || { [ "$want" != 0 ] && [ $got -ne 0 ]; }; then
        echo "ok   $name"
    else
        echo "FAIL $name(退出码 $got)"; sed 's/^/     /' "$work/out"; fails=$((fails + 1))
    fi
}
expect() {   # expect <文件> <正则>:文件里必须有匹配行
    if grep -qE -- "$2" "$1"; then echo "ok     $(basename "$1") 含 /$2/"
    else echo "FAIL   $(basename "$1") 不含 /$2/:"; sed 's/^/     /' "$1" 2>/dev/null || true; fails=$((fails + 1)); fi
}
expect_not() {
    if grep -qE -- "$2" "$1"; then echo "FAIL   $(basename "$1") 不该含 /$2/"; fails=$((fails + 1))
    else echo "ok     $(basename "$1") 不含 /$2/"; fi
}
nix_file() { echo "$NIXOS_CONFIG/home/features/desktop/osmosis.nix"; }
sri_of() { echo "sha256-$(sha256sum "$FAKE_REL/$1" | cut -d' ' -f1)"; }

apk=osmosis-android-arm64-0.1.15.apk

setup
run "Release 上没有 APK:远程构建、上传、装机、抬 nixos_config" 0 v0.1.15
expect "$FAKE_CALLS" '^ssh pc3 .*v0\.1\.15'
expect "$FAKE_CALLS" "^gh release upload v0\.1\.15 .*$apk .*$apk\.sha256"
expect "$FAKE_REL/$apk.sha256" "^[0-9a-f]{64}  $apk\$"
expect "$FAKE_CALLS" '^adb -s dev install -r '
expect_not "$FAKE_CALLS" '^adb -s mi install'
expect "$work/out" 'SM-S9180.*(未在线|跳过)'
expect "$(nix_file)" 'version = "0\.1\.15";'
expect "$(nix_file)" "hash = \"$(sri_of osmosis-desktop-x86_64-linux)\";"
expect "$(nix_file)" "hash = \"$(sri_of io.github.osmosis.svg)\";"
git -C "$work/case/origin.git" log --oneline -1 > "$work/pushed"
expect "$work/pushed" '0\.1\.15'
expect "$work/out" 'ghcr\.io/yebei199/osmosis-server:0\.1\.15@sha256:abc'
expect "$work/out" 'nixos-rebuild'

# 第二次跑:APK 已在 Release 上,不再远程构建;nixos_config 已是新版本,不再提交。
before=$(git -C "$work/case/origin.git" rev-parse HEAD)
run "APK 已在 Release 上:沿用,不再构建" 0 v0.1.15
expect_not "$FAKE_CALLS" '^ssh '
expect_not "$FAKE_CALLS" '^gh release upload'
expect "$FAKE_CALLS" '^adb -s dev install -r '
git -C "$work/case/origin.git" rev-parse HEAD > "$work/head"
expect "$work/head" "^$before\$"

setup
FAKE_CERT=ffff run "签名不对:不上传、不装机" 1 v0.1.15
expect_not "$FAKE_CALLS" '^gh release upload'
expect_not "$FAKE_CALLS" 'install -r'

setup
FAKE_CORRUPT_INSTALL=1 run "设备上读回的 APK 哈希不对:报失败" 1 v0.1.15
expect "$work/out" 'dev.*(哈希|sha256)'

setup
echo dirt >> "$(nix_file)"
run "nixos_config 工作树不干净:不动它" 1 v0.1.15
expect "$(nix_file)" 'version = "0\.1\.14";'
expect "$work/out" '不干净'

setup
echo tampered >> "$FAKE_REL/io.github.osmosis.svg"
run "prefetch 与 sha256sums.txt 对不上:不改 nixos_config" 1 v0.1.15
expect "$(nix_file)" 'version = "0\.1\.14";'

setup
FAKE_MANIFEST='{"digest":"sha256:abc","manifests":[{"platform":{"architecture":"amd64"}}]}' \
    run "镜像不含 arm64:报失败" 1 v0.1.15
expect "$work/out" 'arm64'

setup
FAKE_DEVICES="mi=2211133C" ROLLOUT_ADB_HOSTS="192.0.2.7" run "目标设备全离线:扫端口重连,其余照常" 0 v0.1.15
expect "$FAKE_CALLS" '^nmap .*192\.0\.2\.7'
expect "$work/out" 'NP06J.*(未在线|跳过)'
expect "$(nix_file)" 'version = "0\.1\.15";'

[ $fails -eq 0 ] && echo "==> 全部通过" || { echo "==> $fails 处失败"; exit 1; }
