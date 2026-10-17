#!/usr/bin/env bash
# 满载下送声线程不欠载的回归检查(#192)。
#
# 把所有核用普通优先级的忙循环占满，期间用 crates/audio/examples/load_probe 照生产的路子连续
# 放音，断言：
#   1. 欠载(cpal 错误回调报的 xrun)次数为 0;
#   2. 送声线程(cpal_alsa_out)的调度策略是实时(SCHED_FIFO=1 或 SCHED_RR=2)。
#
# 不出声、不碰系统设置：声音只进本脚本临时建的 PipeWire null sink(PIPEWIRE_NODE 指过去),
# 默认输出设备不改;没有声卡的机器也能跑。负载进程关在 systemd-run --user --scope 里，
# 脚本无论怎么退出(包括 Ctrl-C、断言失败)都停掉那个 scope、删掉 sink。
#
# 用法:test/audio-rt-load.sh [秒数，默认 120] [每核几个忙循环，默认 2]
# 需要 PipeWire(pw-cli、pw-link)、rtkit、systemd 用户实例。探针自己会编(cargo build)。
set -euo pipefail

secs=${1:-120}
per_core=${2:-2}
root=$(cd "$(dirname "$0")/.." && pwd)
sink=osmosis-rt-probe-$$
scope=osmosis-rt-load-$$
probe_pid=
log=$(mktemp -t audio-rt-load.XXXXXX)

sink_id() {
    pw-cli ls Node | awk -v name="\"$sink\"" '
        /^\tid / { id = $2; sub(",", "", id) }
        $1 == "node.name" && $3 == name { print id; exit }'
}

cleanup() {
    systemctl --user stop "$scope.scope" 2>/dev/null || true
    if [ -n "$probe_pid" ]; then kill "$probe_pid" 2>/dev/null || true; fi
    local id
    id=$(sink_id || true)
    if [ -n "$id" ]; then pw-cli destroy "$id" >/dev/null 2>&1 || true; fi
    rm -f "$log"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# 1 = SCHED_FIFO,2 = SCHED_RR,0 = 普通。/proc/<pid>/task/<tid>/stat 的第 41 栏;comm 里
# 可能有空格，所以从最后一个 ') ' 之后数(那之后是第 3 栏)。
thread_policy() {
    local pid=$1 name=$2 task
    for task in /proc/"$pid"/task/*; do
        [ "$(cat "$task/comm" 2>/dev/null)" = "$name" ] || continue
        sed 's/.*) //' "$task/stat" | awk '{ print $39 }'
        return 0
    done
    return 1
}

echo "== 编探针"
cargo build --manifest-path "$root/Cargo.toml" -p audio --example load_probe >"$log" 2>&1 \
    || { cat "$log"; exit 1; }
probe="$root/target/debug/examples/load_probe"

echo "== 建 null sink $sink"
pw-cli create-node adapter "{ factory.name=support.null-audio-sink node.name=$sink \
    media.class=Audio/Sink object.linger=true audio.position=[FL FR] }" >/dev/null
for _ in $(seq 20); do [ -n "$(sink_id)" ] && break; sleep 0.1; done
[ -n "$(sink_id)" ] || { echo "FAIL: null sink 没建出来"; exit 1; }

hogs=$(( $(nproc) * per_core ))
echo "== 起负载:$hogs 个忙循环(nice 0),scope $scope"
if [ "$hogs" -gt 0 ]; then
    systemd-run --user --scope --unit="$scope" --quiet -- \
        bash -c "for i in \$(seq $hogs); do (while :; do :; done) & done; wait" &
    sleep 2
fi

echo "== 放 ${secs}s"
PIPEWIRE_NODE=$sink "$probe" "$secs" >"$log" 2>&1 &
probe_pid=$!

policy=
for _ in $(seq 50); do
    policy=$(thread_policy "$probe_pid" cpal_alsa_out || true)
    [ -n "$policy" ] && break
    sleep 0.1
done
sleep 2
# 声音真的进了 null sink,而不是用户的扬声器
linked=$(pw-link -l | grep -c "$sink" || true)

# PipeWire 那一侧的旁证：sink 与探针两个节点的 ERR(PipeWire 自己数的欠载)。只作参考，不断言
sleep $(( secs > 10 ? secs - 8 : 1 ))
pw_side=$(timeout 10 pw-top -b -n 2 2>/dev/null | grep -E "$sink|load_probe|ALSA plug" | tail -n 3 || true)

wait "$probe_pid" || true
probe_pid=
cat "$log"

xruns=$(sed -n 's/^done xruns=//p' "$log")
fail=0
echo "== 结果"
echo "送声线程调度策略:${policy:-没找到 cpal_alsa_out}(1=FIFO 2=RR 0=普通)"
echo "接到 null sink 的链接行数:$linked"
echo "欠载:${xruns:-探针没跑完}"
echo "PipeWire 那一侧(ERR 栏):"
echo "${pw_side:-  没取到}"
[ "$linked" -gt 0 ] || { echo "FAIL: 探针没接到 null sink"; fail=1; }
[ "${policy:-}" = 1 ] || [ "${policy:-}" = 2 ] || { echo "FAIL: 送声线程不是实时策略"; fail=1; }
[ "${xruns:-x}" = 0 ] || { echo "FAIL: 有欠载"; fail=1; }
[ "$fail" = 0 ] && echo PASS
exit "$fail"
