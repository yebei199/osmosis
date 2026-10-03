#!/usr/bin/env bash
# 累计验收保持默认全量；指定 download 只运行桌面下载的独占用户路径。
set -euo pipefail
case "${1:-all}" in
  all) exec nix-shell test/remote-behavior/env.nix --run 'bash test/remote-behavior/run.sh all' ;;
  download) exec nix-shell test/remote-behavior/env.nix --run 'bash test/remote-behavior/run.sh download' ;;
  *) echo 'usage: acceptance/run.sh [all|download]' >&2; exit 2 ;;
esac
