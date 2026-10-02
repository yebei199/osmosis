#!/usr/bin/env bash
# 完整 CI 入口；所有资源由 pytest 世界登记，失败仍保留证据。
set -euo pipefail
repo_root=$(git rev-parse --show-toplevel)
suite_dir="$repo_root/test/remote-behavior"
cd "$repo_root"
mode=${1:-all}
if [[ "$mode" != all && "$mode" != list ]]; then
    echo 'usage: run.sh [all|list]' >&2
    exit 2
fi
: "${REMOTE_BEHAVIOR_PYTHON:?use the suite Nix shell to select a pidfd-capable Python}"
for required in cargo uv pasta pulseaudio pactl parec initdb postgres pg_isready Xvfb unshare nsenter mount ip dbus-daemon; do
    command -v "$required" >/dev/null || { echo "missing dependency: $required" >&2; exit 2; }
done
if [[ $(id -u) == 0 ]]; then
    echo 'run as an unprivileged user; initdb and private user namespaces are required' >&2
    exit 2
fi
export REMOTE_BEHAVIOR_ARTIFACTS
artifact_root=${REMOTE_BEHAVIOR_ARTIFACT_ROOT:-${XDG_CACHE_HOME:-$HOME/.cache}/osmosis-remote-behavior}
mkdir -p "$artifact_root"
REMOTE_BEHAVIOR_ARTIFACTS=$(mktemp -d "$artifact_root/175-rb.XXXXXXXX")
echo "artifacts=$REMOTE_BEHAVIOR_ARTIFACTS"
if [[ -n ${GITHUB_OUTPUT:-} ]]; then
    echo "artifacts=$REMOTE_BEHAVIOR_ARTIFACTS" >> "$GITHUB_OUTPUT"
fi
export REMOTE_BEHAVIOR_COMMIT
REMOTE_BEHAVIOR_COMMIT=$(git rev-parse HEAD)
if [[ -n $(git status --porcelain --untracked-files=no) ]]; then
    echo 'fixed snapshot requires a committed, clean tracked tree' >&2
    exit 2
fi
export REMOTE_BEHAVIOR_TARGET_DIR="${REMOTE_BEHAVIOR_TARGET_DIR:-$REMOTE_BEHAVIOR_ARTIFACTS/build}"
export SLINT_EMIT_DEBUG_INFO=1
unset OSMOSIS_API_BASE SLINT_LIVE_PREVIEW
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export REMOTE_BEHAVIOR_JOBS="${REMOTE_BEHAVIOR_JOBS:-1}"
if ! [[ "$REMOTE_BEHAVIOR_JOBS" =~ ^[1-9][0-9]*$ ]]; then
    echo 'REMOTE_BEHAVIOR_JOBS must be a positive integer' >&2
    exit 2
fi
date -u +%FT%TZ > "$REMOTE_BEHAVIOR_ARTIFACTS/start.txt"
source "$suite_dir/lifecycle.sh"
suite_python=$(uv run --project "$suite_dir" --frozen --no-managed-python \
    --python "$REMOTE_BEHAVIOR_PYTHON" python -c 'import sys; print(sys.executable)')
"$suite_python" - <<'PY' > "$REMOTE_BEHAVIOR_ARTIFACTS/python-capabilities.log" 2>&1
import os
import signal
import sys

import grpc
import numpy
import psycopg
import pytest

assert hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"), "Python lacks pidfd"
fd = os.pidfd_open(os.getpid())
try:
    signal.pidfd_send_signal(fd, 0)
finally:
    os.close(fd)
print(sys.executable, sys.version)
print("pidfd and grpc/numpy/psycopg/pytest imports available")
PY
git rev-parse HEAD > "$REMOTE_BEHAVIOR_ARTIFACTS/candidate.txt"
sha256sum Cargo.lock "$suite_dir/uv.lock" server/proto/music/v1/music.proto > "$REMOTE_BEHAVIOR_ARTIFACTS/input-sha256.txt"
for required in cargo uv pasta pulseaudio pactl parec initdb postgres pg_isready Xvfb unshare nsenter mount ip dbus-daemon; do
    executable=$(command -v "$required")
    sha256sum "$executable" >> "$REMOTE_BEHAVIOR_ARTIFACTS/environment-sha256.txt"
done
run_owned build 3600 cargo build --locked --config 'profile.dev.package."*".opt-level=0' \
    -p app-desktop -p server --features app-desktop/mcp \
    --target-dir "$REMOTE_BEHAVIOR_TARGET_DIR"
sha256sum "$REMOTE_BEHAVIOR_TARGET_DIR/debug/osmosis-desktop" \
    "$REMOTE_BEHAVIOR_TARGET_DIR/debug/server" > "$REMOTE_BEHAVIOR_ARTIFACTS/binary-sha256.txt"
tests=("$suite_dir")
if [[ "$mode" == list ]]; then tests=("$suite_dir/test_remote.py::test_core_pick[list]"); fi
run_owned pytest 7200 "$suite_python" -m pytest --rootdir "$suite_dir" \
    "${tests[@]}" -n "$REMOTE_BEHAVIOR_JOBS" --max-worker-restart=0 \
    --basetemp "$REMOTE_BEHAVIOR_ARTIFACTS/pytest-tmp" \
    --junitxml "$REMOTE_BEHAVIOR_ARTIFACTS/junit.xml"
