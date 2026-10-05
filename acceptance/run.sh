#!/usr/bin/env bash
# 累计验收与独立 UI 套件共用入口，开发时可只选状态条回归。
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

run_status_ui() {
    nix-shell slint.nix --run 'cargo nextest run --locked -p ui --test group_capsule --test status_strips --test banner --test download --test login --test player_bar --test player_bar_reserve --test lyrics_page --test music_list_layout --config-file acceptance/nextest.toml --profile status-ui --no-tests fail'
}

case "${1:-all}" in
    ui|status-ui) run_status_ui ;;
    download)
        nix-shell test/remote-behavior/env.nix --run 'bash test/remote-behavior/run.sh download'
        ;;
    # 全绿记进 last-green，供 nightly.sh 算可疑区间、release-gate.sh 卡发版。
    all)
        nix-shell test/remote-behavior/env.nix --run 'bash test/remote-behavior/run.sh all'
        run_status_ui
        state=${OSMOSIS_NIGHTLY_STATE:-${XDG_CACHE_HOME:-$HOME/.cache}/osmosis-nightly}
        mkdir -p "$state"
        git rev-parse HEAD > "$state/last-green"
        ;;
    # 门禁按 run.toml [select] 挑出子集后调这里：跑点名的 pytest 节点、Rust 投影与 status-ui，
    # 写出与 all 相同的 JUnit。
    subset)
        shift
        [[ $# -gt 0 ]] || { echo "usage: acceptance/run.sh subset <test>..." >&2; exit 2; }
        nix-shell test/remote-behavior/env.nix --run "$(printf '%q ' bash test/remote-behavior/run.sh subset "$@")"
        run_status_ui
        ;;
    # 手动用：按 base..head 挑选，再按结果走 all 或 subset。理由落 results/selection.json。
    changed)
        [[ $# == 3 ]] || { echo "usage: acceptance/run.sh changed <base> <head>" >&2; exit 2; }
        mkdir -p test/remote-behavior/results
        mapfile -t suite_args < <(nix-shell test/remote-behavior/env.nix --run "$(printf '%q ' \
            python3 test/remote-behavior/selection.py "$2" "$3" \
            --out test/remote-behavior/results/selection.json --args)")
        [[ ${#suite_args[@]} -gt 0 ]] || { echo "selection produced no run arguments" >&2; exit 2; }
        cat test/remote-behavior/results/selection.json
        exec bash "$0" "${suite_args[@]}"
        ;;
    *) echo "usage: acceptance/run.sh [all|status-ui|download|subset <test>...|changed <base> <head>]" >&2; exit 2 ;;
esac
