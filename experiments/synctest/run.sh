#!/usr/bin/env bash
# synctest 一键跑(#137 ②)。在仓库根或本目录下跑都行。
#
#   run.sh build                      编电脑版与安卓版(编译要在用户不在的机器上)
#   run.sh push                       把安卓版与测试媒体推到小米
#   run.sh self <输出目录> [秒]        自校准:服务端与播放端都在小米上,两路都从小米扬声器出
#   run.sh pair <输出目录> [秒]        电脑出 L、小米出 R,小米录音(小米当服务端)
#
# 环境变量:
#   ANDROID_SERIAL  小米 13 的序列号(必填:平板常同时在线)
#   DIST_L DIST_R   L/R 那台的扬声器到小米麦克风的距离,米(pair 缺省 1.0 / 0.03)
#   SEEK_AT SEEK_BY 计划里在第几秒 seek、跳多少秒(缺省 120 / 37.3;设 SEEK_AT= 关掉)
#   START_IN        几秒后起播(缺省 10)
#   PC_HOST         电脑侧在哪台机器上出声(缺省本机;给了就经 ssh 在那台上跑)
#   GAIN            两端的输出增益(缺省 1;0 = 不出声的功能验证,录音里不会有标记)
#
# 产物:<输出目录>/rec.wav(小米录音)、serve.log、play.log、record.log、analysis/summary.json 与 pairs.csv。
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
DEV_DIR=/data/local/tmp/synctest
HOST_BIN="$HERE/target/release/synctest"
ANDROID_BIN="$HERE/target/aarch64-linux-android/release/synctest"
MEDIA="$HERE/target/synctest.wav"
START_IN="${START_IN:-10}"
GAIN="${GAIN:-1}"
SEEK_AT="${SEEK_AT-120}"
SEEK_BY="${SEEK_BY:-37.3}"

adb_() { adb -s "${ANDROID_SERIAL:?要 ANDROID_SERIAL:小米 13 的序列号}" "$@"; }

python_() {
  nix shell --impure --expr 'with import <nixpkgs> {}; python3.withPackages (p: [p.numpy p.scipy])' \
    -c python3 "$@"
}

seek_args() {
  [ -n "$SEEK_AT" ] && echo "--seek-at $SEEK_AT --seek-by $SEEK_BY" || true
}

cmd_build() {
  (cd "$HERE" && nix-shell "$ROOT/slint.nix" --run 'cargo test --release && cargo build --release')
  # 安卓那份要 API 28 的 libaaudio(input_preset),Android.nix 缺省链的是 26。
  (cd "$HERE" && nix-shell "$ROOT/Android.nix" --run '
    export CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER=
    export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android34-clang"
    cargo build --release --target aarch64-linux-android')
  "$HOST_BIN" media --out "$MEDIA" --secs 900
}

cmd_push() {
  adb_ shell mkdir -p "$DEV_DIR"
  adb_ push "$ANDROID_BIN" "$DEV_DIR/" >/dev/null
  adb_ push "$MEDIA" "$DEV_DIR/" >/dev/null
  echo "已推到 $DEV_DIR"
}

# 在小米上后台跑一条命令,输出落到本机文件。本机那条 adb 进程的 pid 记进 PIDS。
# 不用 `pid=$(phone_bg ...)`:命令替换跑在子 shell 里,后台进程成了子 shell 的孩子,
# 这里的 `wait` 等不到它,会立刻返回。
PIDS=()
phone_bg() {
  local log=$1; shift
  adb_ shell "cd $DEV_DIR && $*" > "$log" 2>&1 &
  PIDS+=($!)
}

analyze() {
  local out=$1 dl=$2 dr=$3
  adb_ pull "$DEV_DIR/rec.wav" "$out/rec.wav" >/dev/null
  local seek=()
  [ -n "$SEEK_AT" ] && seek=(--seek-at "$SEEK_AT")
  python_ "$HERE/analyze.py" "$out/rec.wav" --dist-l "$dl" --dist-r "$dr" "${seek[@]}" --out "$out/analysis" | tee "$out/analysis.txt"
}

cmd_self() {
  local out=$1 secs=${2:-180}
  mkdir -p "$out"
  local rec_secs=$(( secs + START_IN + 8 ))
  phone_bg "$out/record.log" ./synctest record --out rec.wav --secs "$rec_secs"
  sleep 1
  phone_bg "$out/serve.log" ./synctest serve --media synctest.wav --channel 0 --port 7010 \
    --start-in "$START_IN" --secs "$secs" --dev phone-L --gain "$GAIN" $(seek_args)
  sleep 1
  phone_bg "$out/play.log" ./synctest play --server 127.0.0.1:7010 --media synctest.wav \
    --channel 1 --dev phone-R --gain "$GAIN"
  wait "${PIDS[@]}" || true
  analyze "$out" 0.0 0.0
}

# 电脑侧那条命令:本机直接跑;PC_HOST 指了别的机器就先把二进制、媒体与它依赖的 nix 库
# 送过去再经 ssh 跑(二进制的 RUNPATH 指着 nix store,那台上得有同样的路径)。
pc_play() {
  local log=$1; shift
  if [ -z "${PC_HOST:-}" ]; then
    "$HOST_BIN" play --media "$MEDIA" "$@" > "$log" 2>&1 &
  else
    ssh "$PC_HOST" "cd /tmp/synctest && ./synctest play --media synctest.wav $*" > "$log" 2>&1 &
  fi
  PIDS+=($!)
}

pc_prepare() {
  [ -z "${PC_HOST:-}" ] && return
  # shellcheck disable=SC2046
  nix-copy-closure --to "$PC_HOST" $(ldd "$HOST_BIN" | grep -oE '/nix/store/[^/]+' | sort -u) >/dev/null
  ssh "$PC_HOST" mkdir -p /tmp/synctest
  scp -q "$HOST_BIN" "$MEDIA" "$PC_HOST:/tmp/synctest/"
}

cmd_pair() {
  local out=$1 secs=${2:-300}
  mkdir -p "$out"
  pc_prepare
  # 小米当服务端:电脑的防火墙挡入站,而电脑连出去、收回包都放行。计划的权威在哪一端
  # 不影响测的东西。无线 adb 的序列号就是小米的 IP:端口。
  local phone_ip=${ANDROID_SERIAL%%:*}
  local rec_secs=$(( secs + START_IN + 8 ))
  phone_bg "$out/record.log" ./synctest record --out rec.wav --secs "$rec_secs"
  sleep 1
  phone_bg "$out/serve.log" ./synctest serve --media synctest.wav --channel 1 --port 7010 \
    --start-in "$START_IN" --secs "$secs" --dev phone-R --gain "$GAIN" $(seek_args)
  sleep 1
  pc_play "$out/play.log" --server "$phone_ip:7010" --channel 0 --dev pc-L --gain "$GAIN"
  wait "${PIDS[@]}" || true
  analyze "$out" "${DIST_L:-1.0}" "${DIST_R:-0.03}"
}

case "${1:-}" in
  build) cmd_build ;;
  push) cmd_push ;;
  self) shift; cmd_self "$@" ;;
  pair) shift; cmd_pair "$@" ;;
  *) sed -n '2,18p' "$0"; exit 2 ;;
esac
