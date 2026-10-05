#!/usr/bin/env bash
# 发版前检查：候选提交必须恰好是本机每晚全量最近一次全绿的提交。
# 状态目录与 nightly.sh 相同，所以要在跑过 nightly.sh 的那台机器上执行。
set -euo pipefail
state=${OSMOSIS_NIGHTLY_STATE:-${XDG_CACHE_HOME:-$HOME/.cache}/osmosis-nightly}
candidate=$(git rev-parse "${1:-HEAD}^{commit}")
if [[ ! -s $state/last-green ]]; then
    echo "release gate: no green full run in $state; run test/remote-behavior/nightly.sh on $candidate" >&2
    exit 1
fi
green=$(cat "$state/last-green")
if [[ $green != "$candidate" ]]; then
    echo "release gate: last green full run is $green, candidate is $candidate; run nightly.sh on the candidate" >&2
    exit 1
fi
echo "release gate: $candidate passed the full suite"
