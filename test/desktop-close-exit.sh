#!/usr/bin/env bash
# 关窗后进程在期限内干净退出吗(#15、#132)。
#
#   test/desktop-close-exit.sh <二进制> [focused|unfocused]...
#
# 每个模式起一个实例、等 Osmosis 窗口出现、按模式摆好窗口,再用 niri 的 close-window
# 关掉它,断言进程 5 秒内退出且退出码为 0(134 是 #15 那个 TLS 析构 abort)。
# unfocused 先把窗口挪到一个空工作区、焦点留在原地 —— #132 是在这种摆法下发现的。
# 超时没退就打出各线程名与它们睡在哪个内核函数上,再 kill 掉接着验下一个。
#
# 二进制要能直接跑,库路径由调用方给(just desktop-exit-check 在 nix-shell 里调它)。
# 要一个在跑的 niri 会话,从 ssh 进来也行(NIRI_SOCKET 默认取运行目录里那个)。
set -uo pipefail

bin=${1:?usage: $0 <binary> [focused|unfocused]...}
shift
modes=("${@:-focused}")
deadline_ds=50 # 十分之一秒;AC-2 的 5 秒

export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
export NIRI_SOCKET=${NIRI_SOCKET:-$(ls "$XDG_RUNTIME_DIR"/niri.*.sock 2>/dev/null | head -1)}
export WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-$(basename "$NIRI_SOCKET" | cut -d. -f2)}
export DBUS_SESSION_BUS_ADDRESS=${DBUS_SESSION_BUS_ADDRESS:-unix:path=$XDG_RUNTIME_DIR/bus} # MPRIS 要它
log=$(mktemp)
trap 'rm -f "$log"' EXIT

osmosis_window() { # 输出这个 pid 的 Osmosis 窗口 id,没有就空
    niri msg --json windows | jq -r --argjson p "$1" '.[] | select(.title=="Osmosis" and .pid==$p) | .id' | head -1
}

alive() { # 僵尸也算走了:退出码还没被 wait 取走而已
    local s
    s=$(awk '{print $3}' "/proc/$1/stat" 2>/dev/null)
    [ -n "$s" ] && [ "$s" != Z ]
}

dump_threads() {
    for t in /proc/"$1"/task/*; do
        printf '     %-16s %s\n' "$(cat "$t/comm")" "$(cat "$t/wchan" 2>/dev/null)"
    done
}

place() { # place <mode> <窗口 id>
    case $1 in
    focused) niri msg action focus-window --id "$2" ;;
    unfocused)
        # 最后一个工作区总是空的;挪过去、焦点不跟。
        local last
        last=$(niri msg --json workspaces | jq -r '[.[].idx] | max')
        niri msg action move-window-to-workspace --window-id "$2" --focus false "$last"
        ;;
    *) echo "未知模式 $1" >&2; exit 2 ;;
    esac
}

check() { # check <mode>
    local mode=$1 app id="" code
    "$bin" > "$log" 2>&1 &
    app=$!
    for _ in $(seq 120); do
        id=$(osmosis_window "$app")
        [ -n "$id" ] && break
        alive "$app" || break
        sleep 0.5
    done
    if [ -z "$id" ]; then
        echo "FAIL $mode:没等到窗口"; sed 's/^/     /' "$log"; kill -9 "$app" 2>/dev/null; wait "$app"; return 1
    fi
    place "$mode" "$id"
    sleep 3 # 让它在这个摆法下跑几帧
    niri msg action close-window --id "$id"
    local t0=$SECONDS
    for _ in $(seq $deadline_ds); do alive "$app" || break; sleep 0.1; done
    if alive "$app"; then
        echo "FAIL $mode:关窗 5 秒后 pid $app 还在,线程:"; dump_threads "$app"
        tail -20 "$log" | sed 's/^/     /'
        kill -9 "$app"; wait "$app"; return 1
    fi
    wait "$app"
    code=$?
    if [ $code -ne 0 ]; then
        echo "FAIL $mode:退出码 $code"; tail -20 "$log" | sed 's/^/     /'; return 1
    fi
    echo "ok   $mode:关窗后 $((SECONDS - t0)) 秒内退出,退出码 0"
}

[ -x "$bin" ] || { echo "没有可执行的 $bin" >&2; exit 2; }
fails=0
for m in "${modes[@]}"; do check "$m" || fails=$((fails + 1)); done
[ $fails -eq 0 ] && echo "==> ${#modes[@]} 条都退出了" || { echo "==> $fails 条失败"; exit 1; }
