#!/usr/bin/env bash
# 证据目录保留（#183）：root 下只留最近 N 个 175-rb.* 运行，成功的剥掉构建产物，失败的整目录保留。
# 没有 exit.txt 且一天内改动过的目录当作别人正在跑，既不删也不占名额。
set -euo pipefail
root=${1:-}
keep=${REMOTE_BEHAVIOR_KEEP_RUNS:-3}
running_minutes=1440
if [[ -z "$root" ]]; then
    echo 'retention: refuse empty artifact root' >&2
    exit 2
fi
root=$(realpath -e -- "$root")
if [[ "$root" == / ]]; then
    echo 'retention: refuse filesystem root' >&2
    exit 2
fi
if ! [[ "$keep" =~ ^[1-9][0-9]*$ ]]; then
    echo 'retention: REMOTE_BEHAVIOR_KEEP_RUNS must be a positive integer' >&2
    exit 2
fi
kept=0
# -type d 不跟随软链，名额按目录 mtime 从新到旧分配。
while IFS= read -r -d '' entry; do
    run=${entry#* }
    if [[ ! -f "$run/exit.txt" && -n $(find "$run" -maxdepth 0 -mmin "-$running_minutes") ]]; then
        continue
    fi
    if ((kept >= keep)); then
        rm -rf -- "$run"
        continue
    fi
    kept=$((kept + 1))
    if [[ $(cat "$run/exit.txt" 2>/dev/null) == 0 ]]; then
        # 与 CI 上传证据时排除的路径一致：主构建、变异子运行的构建与源码快照。
        rm -rf -- "$run/build" "$run"/fault-*/build "$run"/fault-*/snapshot
    fi
done < <(find "$root" -mindepth 1 -maxdepth 1 -type d -name '175-rb.*' -printf '%T@ %p\0' | sort -z -rn)
