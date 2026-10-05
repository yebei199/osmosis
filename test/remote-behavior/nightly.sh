#!/usr/bin/env bash
# 每晚全量入口：跑 acceptance/run.sh all，也就是合回门禁那一条路径——它顺带插桩出覆盖地图，
# 全绿时自己记 last-green。这里只多记一行历史、留下地图，失败时打印可疑区间。
# 调度归调用方（timer 等），只认已提交的 HEAD。状态目录 OSMOSIS_NIGHTLY_STATE：
#   last-green  最近一次全绿的提交；history.tsv  每轮的时间、提交、结果；runs/  每轮日志与地图。
# OSMOSIS_NIGHTLY_PRINT_MAP=1 时把地图打到标准输出，供 remote-run 这类只带回输出的调用取回。
set -euo pipefail
repo=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
cd "$repo"
state=${OSMOSIS_NIGHTLY_STATE:-${XDG_CACHE_HOME:-$HOME/.cache}/osmosis-nightly}
export OSMOSIS_NIGHTLY_STATE=$state
commit=$(git rev-parse HEAD)
run="$state/runs/$(date -u +%Y%m%dT%H%M%SZ)-${commit:0:12}"
mkdir -p "$run"
export REMOTE_BEHAVIOR_ARTIFACT_ROOT=${REMOTE_BEHAVIOR_ARTIFACT_ROOT:-$run/artifacts}
last_green=$(cat "$state/last-green" 2>/dev/null || echo none)
echo "nightly commit=$commit last_green=$last_green run=$run"

status=0
bash acceptance/run.sh all > "$run/suite.log" 2>&1 || status=$?
printf '%s\t%s\t%s\n' "$(date -u +%FT%TZ)" "$commit" "$status" >> "$state/history.tsv"
if [[ $status != 0 ]]; then
    tail -n 40 "$run/suite.log"
    echo "nightly FAILED exit=$status commit=$commit logs=$run" >&2
    if [[ $last_green == none ]]; then
        echo "no earlier green run recorded; suspect range unknown" >&2
    else
        echo "suspect range $last_green..$commit:" >&2
        git log --oneline "$last_green..$commit" >&2 || echo "(last green not in this clone)" >&2
    fi
    exit "$status"
fi
cp test/remote-behavior/results/coverage-map.json "$run/coverage-map.json"
echo "nightly GREEN commit=$commit map=$run/coverage-map.json"
if [[ ${OSMOSIS_NIGHTLY_PRINT_MAP:-} == 1 ]]; then
    echo '--- coverage-map.json ---'
    cat "$run/coverage-map.json"
    echo '--- end coverage-map.json ---'
fi
