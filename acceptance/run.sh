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
    all)
        nix-shell test/remote-behavior/env.nix --run 'bash test/remote-behavior/run.sh all'
        run_status_ui
        ;;
    *) echo "usage: acceptance/run.sh [all|status-ui|download]" >&2; exit 2 ;;
esac
