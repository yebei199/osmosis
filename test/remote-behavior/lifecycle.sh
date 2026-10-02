#!/usr/bin/env bash
# run.sh 与轻量取消测试共用；调用方提供 suite_dir、suite_python 与独占 artifacts。
active_pid=''
active_cancel=''
finish() {
    result=$?
    trap - EXIT TERM INT
    if [[ -n "$active_pid" ]]; then
        cancellation=143
        if [[ "$result" == 130 ]]; then cancellation=130; fi
        echo "$cancellation" > "$active_cancel"
        if wait "$active_pid"; then cleanup_status=0; else cleanup_status=$?; fi
        echo "$cleanup_status" > "$REMOTE_BEHAVIOR_ARTIFACTS/cancel-wait.txt"
        if [[ "$cleanup_status" != "$cancellation" && "$cleanup_status" != 0 ]]; then result=2; fi
    fi
    if [[ -f "$REMOTE_BEHAVIOR_ARTIFACTS/junit.xml" ]]; then
        mkdir -p "$suite_dir/results"
        cp "$REMOTE_BEHAVIOR_ARTIFACTS/junit.xml" "$suite_dir/results/junit.xml"
    fi
    if [[ -n ${projection_junit:-} && -f "$projection_junit" ]]; then
        mkdir -p "$suite_dir/results"
        cp "$projection_junit" \
            "$REMOTE_BEHAVIOR_ARTIFACTS/projection-junit.xml"
        cp "$REMOTE_BEHAVIOR_ARTIFACTS/projection-junit.xml" "$suite_dir/results/projection-junit.xml"
    fi
    date -u +%FT%TZ > "$REMOTE_BEHAVIOR_ARTIFACTS/end.txt"
    echo "$result" > "$REMOTE_BEHAVIOR_ARTIFACTS/exit.txt"
    echo "exit=$result artifacts=$REMOTE_BEHAVIOR_ARTIFACTS"
    exit "$result"
}
trap finish EXIT
trap 'exit 143' TERM
trap 'exit 130' INT
run_owned() {
    name=$1
    timeout=$2
    shift 2
    active_cancel="$REMOTE_BEHAVIOR_ARTIFACTS/$name.cancel"
    "$suite_python" "$suite_dir/lifecycle.py" --directory "$REMOTE_BEHAVIOR_ARTIFACTS" \
        --name "$name" --timeout "$timeout" --cancel "$active_cancel" -- "$@" &
    active_pid=$!
    if wait "$active_pid"; then status=0; else status=$?; fi
    active_pid=''
    active_cancel=''
    return "$status"
}
