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
    # 按 base..head 挑累计场景子集；认不出改动时 selection.py 退回全量。理由落 results/selection.json。
    changed)
        [[ $# == 3 ]] || { echo "usage: acceptance/run.sh changed <base> <head>" >&2; exit 2; }
        mkdir -p test/remote-behavior/results
        mapfile -t suite_args < <(nix-shell test/remote-behavior/env.nix --run "$(printf '%q ' \
            python3 test/remote-behavior/selection.py "$2" "$3" \
            --out test/remote-behavior/results/selection.json --args)")
        [[ ${#suite_args[@]} -gt 0 ]] || { echo "selection produced no run arguments" >&2; exit 2; }
        cat test/remote-behavior/results/selection.json
        nix-shell test/remote-behavior/env.nix --run "$(printf '%q ' bash test/remote-behavior/run.sh "${suite_args[@]}")"
        run_status_ui
        ;;
    *) echo "usage: acceptance/run.sh [all|status-ui|download|changed <base> <head>]" >&2; exit 2 ;;
esac
