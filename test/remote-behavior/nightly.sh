#!/usr/bin/env bash
# 每晚全量入口：插桩跑完整套验收；全绿记 last-green 并生成覆盖地图，失败打印可疑区间。
# 调度归调用方（timer 等），这里只认已提交的 HEAD。状态目录 OSMOSIS_NIGHTLY_STATE：
#   last-green  最近一次全绿的提交；history.tsv  每轮的时间、提交、结果；runs/  每轮证据。
# OSMOSIS_NIGHTLY_PRINT_MAP=1 时把地图打到标准输出，供 remote-run 这类只带回输出的调用取回。
set -euo pipefail
repo=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
cd "$repo"
suite=test/remote-behavior
state=${OSMOSIS_NIGHTLY_STATE:-${XDG_CACHE_HOME:-$HOME/.cache}/osmosis-nightly}
commit=$(git rev-parse HEAD)
run="$state/runs/$(date -u +%Y%m%dT%H%M%SZ)-${commit:0:12}"
mkdir -p "$run/profiles" "$run/stray" "$run/artifacts"
last_green=$(cat "$state/last-green" 2>/dev/null || echo none)
echo "nightly commit=$commit last_green=$last_green run=$run"

# 只给 remote-behavior 的构建插桩；status-ui 照常构建，不沾包装器。
status=0
REMOTE_BEHAVIOR_COVERAGE="$run/profiles" \
    REMOTE_BEHAVIOR_TARGET_DIR="$run/build" \
    REMOTE_BEHAVIOR_ARTIFACT_ROOT="$run/artifacts" \
    RUSTC_WORKSPACE_WRAPPER="$repo/$suite/coverage-rustc.sh" \
    LLVM_PROFILE_FILE="$run/stray/%p-%m.profraw" \
    nix-shell "$suite/env.nix" --run "bash $suite/run.sh all" > "$run/suite.log" 2>&1 || status=$?
if [[ $status == 0 ]]; then
    bash acceptance/run.sh status-ui > "$run/status-ui.log" 2>&1 || status=$?
fi
printf '%s\t%s\t%s\n' "$(date -u +%FT%TZ)" "$commit" "$status" >> "$state/history.tsv"
if [[ $status != 0 ]]; then
    tail -n 40 "$run/suite.log" "$run/status-ui.log" 2>/dev/null || true
    echo "nightly FAILED exit=$status commit=$commit logs=$run" >&2
    if [[ $last_green == none ]]; then
        echo "no earlier green run recorded; suspect range unknown" >&2
    else
        echo "suspect range $last_green..$commit:" >&2
        git log --oneline "$last_green..$commit" >&2 || echo "(last green not in this clone)" >&2
    fi
    exit "$status"
fi
echo "$commit" > "$state/last-green"
nix-shell "$suite/env.nix" --run "python3 $suite/covmap.py --profiles $run/profiles \
    --junit $suite/results/junit.xml --object $run/build/debug/osmosis-desktop \
    --object $run/build/debug/server --out $run/coverage-map.json"
echo "nightly GREEN commit=$commit map=$run/coverage-map.json"
if [[ ${OSMOSIS_NIGHTLY_PRINT_MAP:-} == 1 ]]; then
    echo '--- coverage-map.json ---'
    cat "$run/coverage-map.json"
    echo '--- end coverage-map.json ---'
fi
