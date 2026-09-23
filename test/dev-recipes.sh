#!/usr/bin/env bash
# debug 配方的守卫与设备选择(#119)。纯 shell 逻辑,用假的 adb / nix-shell 顶在 PATH
# 前面驱动 justfile,不碰真设备、不编译。任何一条断言不成立就非零退出。
#
# 覆盖:dev-adb 的序列号解析(指定 / 唯一在线 / 多台拒绝 / 指定的不在线)、
# android-not-production 认生产平板型号、local-backend-up 探活,以及
# desktop-dev 与 mcp-android 在守卫失败时**不进编译**(假 nix-shell 没被调用),
# 以及 desktop-install 装的是带库路径的启动脚本而不是软链(#112)。
set -euo pipefail

cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'kill "${srv:-}" 2>/dev/null || true; rm -rf "$work"' EXIT

# 假 adb:在线设备取 FAKE_DEVICES(空格分隔),型号取 FAKE_MODEL,其余调用记进 FAKE_CALLS。
mkdir "$work/bin"
cat > "$work/bin/adb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = devices ]; then
    echo "List of devices attached"
    for d in $FAKE_DEVICES; do printf '%s\tdevice\n' "$d"; done
    exit 0
fi
if [ "${4:-} ${5:-}" = "getprop ro.product.model" ]; then
    printf '%s\r\n' "$FAKE_MODEL"   # 真 adb shell 的输出带 \r
    exit 0
fi
echo "adb $*" >> "$FAKE_CALLS"
EOF
# 假 nix-shell:只记账;不编译的 --run(desktop-install 取库路径那条)照跑,库路径取 FAKE_LIBS。
cat > "$work/bin/nix-shell" <<'EOF'
#!/bin/sh
echo "nix-shell $*" >> "$FAKE_CALLS"
case "$3" in *cargo*|'') ;; *) LD_LIBRARY_PATH=$FAKE_LIBS sh -c "$3" ;; esac
EOF
printf '#!/bin/sh\necho "nix-store $*" >> "$FAKE_CALLS"\n' > "$work/bin/nix-store"
chmod +x "$work/bin/"*
export PATH="$work/bin:$PATH" FAKE_CALLS="$work/calls" FAKE_MODEL=2211133C
unset ANDROID_SERIAL

# 应答 /health 的最小后端,端口临时挑。
mkdir "$work/www" && touch "$work/www/health"
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
python3 -m http.server "$port" --bind 127.0.0.1 -d "$work/www" >/dev/null 2>&1 &
srv=$!
for _ in $(seq 50); do curl -sf -o /dev/null "http://127.0.0.1:$port/health" && break; sleep 0.1; done
up="http://127.0.0.1:$port"
down="http://127.0.0.1:1"   # 1 号端口没人监听

fails=0
check() {   # check <描述> <0 = 期望成功 | 1 = 期望失败> -- 命令...
    local name=$1 want=$2; shift 3
    : > "$FAKE_CALLS"
    set +e; "$@" > "$work/out" 2>&1; local got=$?; set -e
    if { [ "$want" = 0 ] && [ $got -eq 0 ]; } || { [ "$want" != 0 ] && [ $got -ne 0 ]; }; then
        echo "ok   $name"
    else
        echo "FAIL $name(退出码 $got)"; sed 's/^/     /' "$work/out"; fails=$((fails + 1))
    fi
}
expect_call() {
    if grep -qE "$1" "$FAKE_CALLS"; then echo "ok     调用含 /$1/"
    else echo "FAIL   调用不含 /$1/:"; sed 's/^/     /' "$FAKE_CALLS"; fails=$((fails + 1)); fi
}
expect_no_build() {
    if grep -q '^nix-shell' "$FAKE_CALLS"; then echo "FAIL   守卫失败后仍进了编译"; fails=$((fails + 1))
    else echo "ok     没进编译"; fi
}
expect_out() {
    if grep -qF "$1" "$work/out"; then echo "ok     输出含「$1」"
    else echo "FAIL   输出不含「$1」"; fails=$((fails + 1)); fi
}

export FAKE_DEVICES="dev1 tab1"
check "多台在线、没指定 → 拒绝并列出设备" 1 -- just dev-adb shell true
expect_out "tab1"
check "指定了在线的那台 → 带 -s 调" 0 -- env ANDROID_SERIAL=dev1 just dev-adb reverse tcp:3000 tcp:3000
expect_call "^adb -s dev1 reverse tcp:3000 tcp:3000$"
check "指定的不在线 → 拒绝" 1 -- env ANDROID_SERIAL=gone just dev-adb shell true

export FAKE_DEVICES="dev1"
check "只有一台 → 用它" 0 -- just dev-adb install -r x.apk
expect_call "^adb -s dev1 install -r x.apk$"

check "开发机型号 → 放行" 0 -- just android-not-production
FAKE_MODEL=NP06J check "生产平板型号 → 拒绝" 1 -- just android-not-production

check "后端在 → 放行" 0 -- just --set local_api "$up" local-backend-up
check "后端不在 → 拒绝并说怎么起" 1 -- just --set local_api "$down" local-backend-up
expect_out "just server-dev"

check "desktop-dev:后端不在 → 编译前失败" 1 -- just --set local_api "$down" desktop-dev
expect_no_build
check "mcp-android:后端不在 → 编译前失败" 1 -- just --set local_api "$down" mcp-android
expect_no_build
FAKE_MODEL=NP06J check "mcp-android:目标是生产平板 → 编译前失败" 1 -- just --set local_api "$up" mcp-android
expect_no_build
check "mcp-android:就绪 → reverse 之后才编译" 0 -- just --set local_api "$up" mcp-android
expect_call "^adb -s dev1 reverse tcp:3000 tcp:3000$"
expect_call "^nix-shell Android.nix"
if [ "$(grep -nE '^adb -s dev1 reverse' "$FAKE_CALLS" | cut -d: -f1)" -lt "$(grep -n '^nix-shell' "$FAKE_CALLS" | cut -d: -f1)" ]; then
    echo "ok     reverse 在编译之前"
else echo "FAIL   reverse 不在编译之前"; fails=$((fails + 1)); fi

export FAKE_LIBS=/nix/store/aaa-vulkan-loader/lib:/nix/store/bbb-wayland/lib
h="$work/home"
check "desktop-install:装好" 0 -- env -u LD_LIBRARY_PATH HOME="$h" XDG_DATA_HOME="$h/share" just desktop-install
w="$h/.local/bin/osmosis-desktop"
if [ -f "$w" ] && [ ! -L "$w" ] && [ -x "$w" ]; then echo "ok     装的是可执行脚本,不是软链"
else echo "FAIL   $w 不是可执行的普通文件"; ls -la "$w" 2>&1 | sed 's/^/     /'; fails=$((fails + 1)); fi
for want in "LD_LIBRARY_PATH=\"$FAKE_LIBS" "exec \"$PWD/target/release/osmosis-desktop\""; do
    if grep -qF "$want" "$w" 2>/dev/null; then echo "ok     脚本含「$want」"
    else echo "FAIL   脚本不含「$want」"; fails=$((fails + 1)); fi
done
expect_call "^nix-store .*/nix/store/aaa-vulkan-loader$"
expect_call "^nix-store .*/nix/store/bbb-wayland$"

[ $fails -eq 0 ] && echo "==> 全部通过" || { echo "==> $fails 条失败"; exit 1; }
