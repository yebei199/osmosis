#!/usr/bin/env bash
# 完整 CI 入口；所有资源由 pytest 世界登记，失败仍保留证据。
set -euo pipefail
repo_root=$(git rev-parse --show-toplevel)
suite_dir="$repo_root/test/remote-behavior"
cd "$repo_root"
mode=${1:-all}
if [[ "$mode" != all && "$mode" != list && "$mode" != radio && "$mode" != download && "$mode" != targeted && "$mode" != subset ]]; then
    echo 'usage: run.sh [all|list|radio|download|targeted <testcase>...|subset <testcase>...]' >&2
    exit 2
fi
selected_tests=()
# subset 是 selection.py 挑出的累计子集，和 all 一样带上 Rust 投影回归。
if [[ "$mode" == targeted || "$mode" == subset ]]; then
    shift
    if [[ $# -eq 0 ]]; then
        echo 'targeted requires at least one testcase' >&2
        exit 2
    fi
    for testcase in "$@"; do
        selected_tests+=("$suite_dir/$testcase")
    done
fi
: "${REMOTE_BEHAVIOR_PYTHON:?use the suite Nix shell to select a pidfd-capable Python}"
for required in cargo cargo-nextest uv pasta pulseaudio pactl parec initdb postgres pg_isready Xvfb unshare nsenter mount ip dbus-daemon llvm-profdata llvm-cov; do
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
# 全局 build.build-dir 按工作区路径哈希把中间产物放到树外；钉回 target，拷贝与种子才带得走热缓存（#189）。
export CARGO_BUILD_BUILD_DIR="$REMOTE_BEHAVIOR_TARGET_DIR"
export SLINT_EMIT_DEBUG_INFO=1
unset OSMOSIS_API_BASE SLINT_LIVE_PREVIEW
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export REMOTE_BEHAVIOR_JOBS="${REMOTE_BEHAVIOR_JOBS:-1}"
if ! [[ "$REMOTE_BEHAVIOR_JOBS" =~ ^[1-9][0-9]*$ ]]; then
    echo 'REMOTE_BEHAVIOR_JOBS must be a positive integer' >&2
    exit 2
fi
date -u +%FT%TZ > "$REMOTE_BEHAVIOR_ARTIFACTS/start.txt"
projection_junit=''
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
for required in cargo cargo-nextest uv pasta pulseaudio pactl parec initdb postgres pg_isready Xvfb unshare nsenter mount ip dbus-daemon llvm-profdata llvm-cov; do
    executable=$(command -v "$required")
    sha256sum "$executable" >> "$REMOTE_BEHAVIOR_ARTIFACTS/environment-sha256.txt"
done
# 全量顺带生成覆盖地图（#185）：只给工作区 crate 插桩，每个世界按 pytest 节点写 profraw。
# 进程之外落下的 profraw（构建脚本、投影测试）收进 stray，不进地图。
if [[ "$mode" == all ]]; then
    export REMOTE_BEHAVIOR_COVERAGE="$REMOTE_BEHAVIOR_ARTIFACTS/coverage"
    mkdir -p "$REMOTE_BEHAVIOR_COVERAGE/stray"
    export RUSTC_WORKSPACE_WRAPPER="$suite_dir/coverage-rustc.sh"
    export LLVM_PROFILE_FILE="$REMOTE_BEHAVIOR_COVERAGE/stray/%p-%m.profraw"
fi
run_owned build 3600 cargo build --locked --config 'profile.dev.package."*".opt-level=0' \
    -p app-desktop -p server --features app-desktop/mcp \
    --target-dir "$REMOTE_BEHAVIOR_TARGET_DIR"
sha256sum "$REMOTE_BEHAVIOR_TARGET_DIR/debug/osmosis-desktop" \
    "$REMOTE_BEHAVIOR_TARGET_DIR/debug/server" > "$REMOTE_BEHAVIOR_ARTIFACTS/binary-sha256.txt"
if [[ "$mode" == all || "$mode" == subset ]]; then
    projection_junit="$repo_root/target/nextest/rb-projection/junit.xml"
    rm -f "$projection_junit"
    run_owned projection 3600 cargo nextest run --locked -p ui -p app-desktop -p app-core -p server \
        --config 'profile.dev.package."*".opt-level=0' \
        --target-dir "$REMOTE_BEHAVIOR_TARGET_DIR" \
        --config-file "$suite_dir/nextest.toml" --profile rb-projection --no-tests fail
    cp "$projection_junit" \
        "$REMOTE_BEHAVIOR_ARTIFACTS/projection-junit.xml"
fi
tests=("$suite_dir")
if [[ "$mode" == list ]]; then tests=("$suite_dir/test_remote.py::test_core_pick[list]"); fi
if [[ "$mode" == radio ]]; then tests=("$suite_dir/test_remote.py::test_radio_publication_preserves_group_reference"); fi
if [[ "$mode" == download ]]; then tests=("$suite_dir/test_download.py"); fi
if [[ "$mode" == targeted || "$mode" == subset ]]; then tests=("${selected_tests[@]}"); fi
run_owned pytest 7200 "$suite_python" -m pytest --rootdir "$suite_dir" \
    "${tests[@]}" -n "$REMOTE_BEHAVIOR_JOBS" --max-worker-restart=0 \
    --basetemp "$REMOTE_BEHAVIOR_ARTIFACTS/pytest-tmp" \
    --junitxml "$REMOTE_BEHAVIOR_ARTIFACTS/junit.xml"
# 只有全绿才产出地图；日志里另打一行压缩副本，远端运行只带回日志时也取得回来。
if [[ "$mode" == all ]]; then
    run_owned covmap 1800 "$suite_python" "$suite_dir/covmap.py" \
        --profiles "$REMOTE_BEHAVIOR_COVERAGE" --junit "$REMOTE_BEHAVIOR_ARTIFACTS/junit.xml" \
        --object "$REMOTE_BEHAVIOR_TARGET_DIR/debug/osmosis-desktop" \
        --object "$REMOTE_BEHAVIOR_TARGET_DIR/debug/server" \
        --out "$REMOTE_BEHAVIOR_ARTIFACTS/coverage-map.json"
    mkdir -p "$suite_dir/results"
    cp "$REMOTE_BEHAVIOR_ARTIFACTS/coverage-map.json" "$suite_dir/results/coverage-map.json"
    echo "coverage-map-gzip-base64: $(gzip -9c "$suite_dir/results/coverage-map.json" | base64 -w0)"
fi
