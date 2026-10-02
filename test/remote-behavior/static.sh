#!/usr/bin/env bash
# 审前只做静态检查；不收集或执行 pytest，不求值或构建 Nix。
set -euo pipefail
repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"
skills_root=${AI_CLI_SKILLS_DIR:-$HOME/.agents/skills}
FINAL_CHECK_PHASE=lint "$skills_root/python/scripts/final-check.sh" test/remote-behavior all
FINAL_CHECK_PHASE=type "$skills_root/python/scripts/final-check.sh" test/remote-behavior all
nixfmt --check test/remote-behavior/env.nix
nix-instantiate --parse test/remote-behavior/env.nix >/dev/null
for shell_file in test/remote-behavior/*.sh; do bash -n "$shell_file"; done
just --list >/dev/null
uv run --project test/remote-behavior --frozen python - <<'PY'
import ast
import tomllib
from pathlib import Path

for path in sorted(Path("test/remote-behavior").glob("*.py")):
    ast.parse(path.read_text(), filename=str(path))
    print(f"AST {path}")
for path in [*sorted(Path("test/remote-behavior").glob("*.toml")), *sorted(Path("acceptance").glob("*.toml"))]:
    tomllib.loads(path.read_text())
    print(f"TOML {path}")
PY
uv run --project test/remote-behavior --frozen python \
    "$skills_root/tdd/scripts/accept-check.py" ids acceptance/175.toml
git diff --check
