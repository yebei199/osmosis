#!/usr/bin/env bash
# #112 的两条验收:`just desktop-install` 装好之后,不借任何 nix shell 的环境也起得来。
#
#   shell  env -i 起的干净环境(只留连合成器要的几个变量)直接跑 ~/.local/bin/osmosis-desktop
#   menu   以用户会话管理器的环境(systemd-run --user)跑 `gtk-launch io.github.osmosis`,
#          即菜单项那条路
#
# 判据不看观感:niri 报出标题为 Osmosis 的窗口,那个进程的 /proc/<pid>/maps 里有
# libvulkan(wgpu 的 dlopen 成功了);shell 那条还查日志里没有 adapter 报错。
# 每条起来的实例事后关掉。已经有 Osmosis 窗口开着就拒绝跑,免得把别人的实例当成自己的。
#
# 要一个在跑的 niri 会话,从 ssh 进来也行(NIRI_SOCKET 默认取运行目录里那个)。
# HOME / XDG_DATA_HOME / XDG_STATE_HOME 与 desktop-install 读同一套:换个前缀就在隔离
# 目录里验,不碰真实的菜单项与装机版。
set -uo pipefail

export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
export NIRI_SOCKET=${NIRI_SOCKET:-$(ls "$XDG_RUNTIME_DIR"/niri.*.sock 2>/dev/null | head -1)}
bin="$HOME/.local/bin"
data=${XDG_DATA_HOME:-$HOME/.local/share}
state=${XDG_STATE_HOME:-$HOME/.local/state}
log=$(mktemp)
trap 'rm -f "$log"' EXIT

osmosis_window() {   # 输出「窗口 id pid」,没有就空
    niri msg --json windows | jq -r '.[] | select(.title=="Osmosis") | "\(.id) \(.pid)"' | head -1
}

launch_shell() {
    env -i HOME="$HOME" PATH=/run/current-system/sw/bin XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
        WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-wayland-1}" XDG_DATA_HOME="$data" XDG_STATE_HOME="$state" \
        "$bin/osmosis-desktop" > "$log" 2>&1 &
}

# 菜单、启动器起应用用的是用户会话管理器那份环境,systemd-run --user 起的服务继承的
# 正是它(而不是本 shell 的)。只补上隔离前缀;PATH 前置的 ~/.local/bin 在真实会话里本来
# 就有。那份环境里要是带着 LD_LIBRARY_PATH,这条就验不出东西,直接判失败。
# ExitType=cgroup:gtk-launch 起完应用就退,默认会连带把单元里的应用收掉。
launch_menu() {
    : > "$log"
    local env
    env=$(systemctl --user show-environment)
    if grep -q '^LD_LIBRARY_PATH=' <<< "$env"; then
        echo "会话环境里有 LD_LIBRARY_PATH,menu 这条验不出东西" > "$log"; return
    fi
    systemd-run --user --collect --quiet -p ExitType=cgroup -E HOME="$HOME" \
        -E XDG_DATA_HOME="$data" -E XDG_STATE_HOME="$state" \
        -E PATH="$bin:$(sed -n 's/^PATH=//p' <<< "$env")" gtk-launch io.github.osmosis
}

check() {   # check <shell|menu>
    local name=$1 win="" ok=0
    "launch_$name"
    for _ in $(seq 60); do
        win=$(osmosis_window)
        [ -n "$win" ] && break
        sleep 0.5
    done
    if [ -z "$win" ]; then
        echo "FAIL $name:30 秒内没有 Osmosis 窗口"; sed 's/^/     /' "$log"; return 1
    fi
    local id=${win% *} pid=${win#* }
    if grep -q libvulkan "/proc/$pid/maps"; then echo "ok   $name:pid $pid 载入了 libvulkan"
    else echo "FAIL $name:pid $pid 没载入 libvulkan"; ok=1; fi
    if grep -q '找不到可用的 wgpu adapter' "$log"; then echo "FAIL $name:日志里有 adapter 报错"; ok=1; fi
    niri msg action close-window --id "$id"
    for _ in $(seq 40); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
    # 关窗后没退的留着会让下一条量到它(pid 相同即是),收掉再往下走。
    if kill -0 "$pid" 2>/dev/null; then
        echo "note $name:关窗 20 秒后 pid $pid 还在,已 kill"; kill "$pid"; sleep 1
    fi
    [ -z "$(osmosis_window)" ] || { echo "FAIL $name:Osmosis 窗口收不掉,后面的判据不可信"; exit 1; }
    return $ok
}

[ -x "$bin/osmosis-desktop" ] || { echo "没有 $bin/osmosis-desktop,先跑 just desktop-install" >&2; exit 2; }
[ -f "$data/applications/io.github.osmosis.desktop" ] || { echo "没有装菜单项,先跑 just desktop-install" >&2; exit 2; }
[ -z "$(osmosis_window)" ] || { echo "已经有 Osmosis 窗口开着,先关掉再验" >&2; exit 2; }

fails=0
check shell || fails=$((fails + 1))
check menu || fails=$((fails + 1))
[ $fails -eq 0 ] && echo "==> 两条都起得来" || { echo "==> $fails 条失败"; exit 1; }
