#!/usr/bin/env bash
# 掉线与重启的验收(#142 的掉线规则,推翻了 #118/#111 那几条「断线就撤锁、遥控器租约」),
# 走真界面、看真相源:
#
#   link-loss    PEER 把 OUT 与本机都设成出声设备,点一首;掐断 OUT 的信令(HTTP 照通)
#                → OUT 立刻停下、PEER 照放、库里组仍在放;恢复 → OUT 照最新状态接着出声。
#   last-output  PEER 只让 OUT 出声,点一首;掐断 OUT → OUT 停下,服务端探活发现它掉了之后
#                把组置为暂停;恢复 → OUT 仍停着(不按旧状态自己放);PEER 按 ⏯ → OUT 出声。
#   server-restart  PEER 只让 OUT 出声,点一首;RESTART_SERVER 重启服务端 → 组状态还在库里、
#                版本不回退,两台重新入册,OUT 照状态接着出声。
#   restarts     两台一起反复重启应用 → 服务端一次限流都没有,每次都重新入册。
#
# 真相源:play_groups 那一行(在不在放、版本号)、服务端日志(入册、出声设备出册、限流),
# 安卓看 dumpsys audio 里本应用有没有 state:started 的 AudioTrack,ns 实例看播放器自己的
# 位置日志在不在走。都不看截图。
#
# 设备用「种类:参数」写:
#   ns:<目录>       namespace 里的桌面实例,由 test/ns-desktop.sh <目录> 起;目录里有 ns.pid
#                   (就是应用进程)与 gate-desk.pid(ns 里那道 signal-gate)。
#   android:<闸pid> 真机,MCP 走 adb forward 的 8090;闸是宿主上那道 signal-gate 的 pid
#                   (adb reverse tcp:3000 指到它)。
#
# 用法:
#   OUT=android:12345 PEER=ns:/path/a SERVER_LOG=/path/server.log test/link-loss-e2e.sh link-loss
#   OUT=... PEER=... SERVER_LOG=... test/link-loss-e2e.sh last-output
#   OUT=... PEER=... SERVER_LOG=... RESTART_SERVER=<重启服务端的命令> test/link-loss-e2e.sh server-restart
#   OUT=... PEER=... SERVER_LOG=... RESTART_OUT=<命令> RESTART_PEER=<命令> ROUNDS=5 test/link-loss-e2e.sh restarts
#
# SERVER_LOG 那份 server 要带 `RUST_LOG=info,server=debug`(入册是 debug 级);ns 实例要带
# `RUST_LOG=info,ui=debug` 起(`playing` 读播放器的位置日志)。RESTART_SERVER 之后的服务端
# 要接着往同一个 SERVER_LOG 里写(追加)。
#
# 前提:两台都登录在同一个账号上(test/mcp-login.sh),连的是 SERVER_LOG 那份 server。
# 出声测试:手机音量调低,ns 实例走 null sink;每条场景出声不超过半分钟,收尾把组暂停。
set -euo pipefail

PG_CONTAINER="${PG_CONTAINER:-osmosis-pg}"
: "${SERVER_LOG:?要 SERVER_LOG:两台连的那份 server 的日志}"

fail() { echo "失败: $*" >&2; exit 1; }

kind() { echo "${1%%:*}"; }
arg() { echo "${1#*:}"; }

# mcp <设备> <工具> <参数 JSON> —— 吐出结果里的 text。
mcp() {
  local body="{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$2\",\"arguments\":$3}}"
  local cmd=(curl -s --noproxy '*' -X POST -H 'Content-Type: application/json'
    -H 'Accept: application/json, text/event-stream' -d "$body")
  case "$(kind "$1")" in
    ns) nsenter -t "$(cat "$(arg "$1")/ns.pid")" -U -n --preserve-credentials \
          "${cmd[@]}" http://127.0.0.1:8091/mcp ;;
    android) "${cmd[@]}" http://127.0.0.1:8090/mcp ;;
  esac | python3 -c 'import json,sys; r=json.load(sys.stdin); print(r["result"]["content"][0].get("text","") if "result" in r else "{}")'
}

window() { mcp "$1" list_windows '{}' | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["windowHandles"][0]))'; }

# handles <设备> <元素 id> —— 每行一个句柄。
handles() {
  mcp "$1" find_elements_by_id "{\"windowHandle\":$(window "$1"),\"elementsId\":\"$2\"}" \
    | python3 -c 'import json,sys; [print(json.dumps(h,separators=(",",":"))) for h in json.load(sys.stdin).get("elementHandles",[])]'
}

click() { mcp "$1" click_element "{\"elementHandle\":$2}" >/dev/null; }

label_of() {
  mcp "$1" get_element_properties "{\"elementHandle\":$2}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("accessibleLabel",""), "checked" if d.get("accessibleChecked") else "")'
}

# gate <设备> cut|heal
gate() {
  local pid
  case "$(kind "$1")" in
    ns) pid=$(cat "$(arg "$1")/gate-desk.pid") ;;
    android) pid=$(arg "$1") ;;
  esac
  kill "-$([ "$2" = cut ] && echo USR1 || echo USR2)" "$pid"
}

# 设备在名册里叫什么:「主机名 #进程号」(见 ui::sync::syncplay::identity)。
name_of() {
  case "$(kind "$1")" in
    ns) echo "$(cat /etc/hostname) #$(cat "$(arg "$1")/ns.pid")" ;;
    android) echo "device #$(adb shell pidof io.github.osmosis | tr -d '\r')" ;;
  esac
}

# 个人页:输出芯片摆在那里(已经开着就不动)。桌面是侧栏底部头像那颗 RoundControl,安卓是底栏第三格。
open_profile() {
  # 头像那颗键是开关:已经在个人页上时再按一下反而把它关掉。
  [ -n "$(handles "$1" OutputChip::touch)" ] && return
  case "$(kind "$1")" in
    ns) click "$1" "$(handles "$1" RoundControl::touch | head -1)" ;;
    android) click "$1" "$(handles "$1" NavItem::touch | sed -n 3p)" ;;
  esac
  sleep 1.5
}

# chip <设备> <标签> —— 标签完全一致的那颗输出芯片的句柄。
chip() {
  local h
  for h in $(handles "$1" OutputChip::touch); do
    [[ "$(label_of "$1" "$h")" == "输出到 $2"* ]] && { echo "$h"; return; }
  done
  return 1
}

chip_checked() { [[ "$(label_of "$1" "$(chip "$1" "$2")")" == *checked ]]; }

# toggle <设备> <名字> —— 那台设备旁「加入 / 移出」小键的句柄。
toggle() {
  local h
  for h in $(handles "$1" MemberToggle::touch); do
    [[ "$(label_of "$1" "$h")" == ?"入 $2"* || "$(label_of "$1" "$h")" == ?"出 $2"* ]] && { echo "$h"; return; }
  done
  return 1
}

# until_true <秒> <描述> <命令...>
until_true() {
  local deadline=$((SECONDS + $1)) what=$2; shift 2
  until "$@"; do
    [ $SECONDS -lt $deadline ] || fail "等 $what 超时"
    sleep 1
  done
  echo "  ✓ $what"
}

plays() {
  docker exec "$PG_CONTAINER" psql -U slint -d osmosis -tAc "select count(*) from play_events;" | tr -d '[:space:]'
}

# 在这台上点一首歌(音乐页 → 列表 → 第一或第二行),断言 play_events 多一行。
# 在组里时这一下只改全局状态,起播由服务端记,一样多一行。
play_locally() {
  local dev=$1 before idx
  before=$(plays)
  click "$dev" "$(handles "$dev" NavItem::touch | sed -n 2p)"; sleep 1.5
  click "$dev" "$(handles "$dev" WallView::view-list-btn | head -1)"; sleep 1.5
  # 同一行正在放时再点是多余的点击,不会重新起播 —— 两行轮着点,见 pick-e2e.sh。
  for idx in 1 2; do
    # 点一下就起播。别连点两下:2026-09-23 在 ns 桌面实例上实测,同一行连点两下
    # 什么都没放,状态回到「点一首歌开始」(#111 回报里记为待裁决发现)。
    click "$dev" "$(handles "$dev" TrackList::touch | sed -n "${idx}p")"
    for _ in $(seq 20); do
      sleep 1
      if [ "$(plays)" -gt "$before" ]; then
        echo "  ✓ 起播落账(play_events $before → $(plays))"
        return 0
      fi
    done
  done
  fail "$dev 本机点歌 40 秒没落账"
}

# 本机这个应用此刻在不在出声。只认本应用那个进程 —— 别的应用在放也会有声音。
playing() {
  local pid
  case "$(kind "$1")" in
    android)
      pid=$(adb shell pidof io.github.osmosis | tr -d '\r')
      adb shell dumpsys audio | grep -qE "u/pid:[0-9]+/$pid state:started" ;;
    ns)
      # 播放器自己每秒记一行「自动续播轮询: 位置 …, 放空 …」(ui=debug,ns-desktop.sh
      # 要带 RUST_LOG=info,ui=debug 起)。隔两秒位置变了、且没放空,才算在出声。
      # 不看 pipewire:应用一直开着输出流,没在放歌时那条流照样是 running。
      local log a b
      log="$(arg "$1")/desk.log"
      a=$(grep "自动续播轮询" "$log" | tail -1)
      sleep 2
      b=$(grep "自动续播轮询" "$log" | tail -1)
      [[ "$b" == *"放空 false"* && "${a#*位置 }" != "${b#*位置 }" ]] ;;
  esac
}

log_count() { grep -c "$1" "$SERVER_LOG" || true; }

joins() { log_count "设备入册"; }
joined_since() { [ "$(joins)" -gt "$1" ]; }
throttles() { log_count "限流挡下一条请求"; }

sql() { docker exec "$PG_CONTAINER" psql -U slint -d osmosis -tAc "$1" | tr -d '[:space:]'; }
# 组的全局状态:在不在放(t/f)、版本号。一个账号一行,取最近改过的那一行。
group_playing() { [ "$(sql "select playing from play_groups order by version desc limit 1;")" = t ]; }
group_paused() { ! group_playing; }
group_version() { sql "select coalesce(max(version), 0) from play_groups;"; }

silent() { ! playing "$1"; }

# PEER 让 OUT 出声(点 OUT 那颗芯片);WITH_PEER=1 时再按「加入 本机」,两台一起出声。
# 然后在 PEER 上点一首歌 —— 在组里点歌只改全局状态,起播记账由服务端记。
form_group() {
  local out_name; out_name=$(name_of "$OUT")
  open_profile "$PEER"
  click "$PEER" "$(chip "$PEER" "$out_name")"
  sleep 2
  if [ "${WITH_PEER:-}" = 1 ]; then
    open_profile "$PEER"
    click "$PEER" "$(toggle "$PEER" 本机)"
    sleep 2
  fi
  play_locally "$PEER"
  until_true 30 "OUT 在出声" playing "$OUT"
}

# ⏯ 那颗键:控制条上标签是「暂停」或「播放」的 RoundControl。
play_key() {
  local h
  for h in $(handles "$1" RoundControl::touch); do
    [[ "$(label_of "$1" "$h")" == 暂停* || "$(label_of "$1" "$h")" == 播放* ]] && { echo "$h"; return; }
  done
  return 1
}

# 收尾:把组暂停,不留设备在响。
rest() {
  group_playing || return 0
  click "$PEER" "$(play_key "$PEER")" 2>/dev/null || true
}

link_loss() {
  echo "== link-loss:$PEER 让 $OUT 与自己一起出声,掐 OUT 的信令"
  WITH_PEER=1 form_group
  until_true 30 "PEER 也在出声" playing "$PEER"

  gate "$OUT" cut
  until_true 10 "OUT 断线后立刻停下" silent "$OUT"
  playing "$PEER" || fail "OUT 断线时 PEER 也停了"
  group_playing || fail "OUT 断线时组被暂停了(PEER 还在出声,不该停)"
  echo "  ✓ PEER 照放,组仍在放"

  local before; before=$(joins)
  gate "$OUT" heal
  until_true 30 "OUT 重新入册" joined_since "$before"
  until_true 30 "OUT 照最新状态接着出声" playing "$OUT"
}

last_output() {
  echo "== last-output:$PEER 只让 $OUT 出声,掐 OUT 的信令"
  form_group
  local left; left=$(log_count "出声设备出册")

  gate "$OUT" cut
  until_true 10 "OUT 断线后立刻停下" silent "$OUT"
  # 客户端那一半断了,服务端要等探活(30 秒一次、容忍两次)才发现。
  until_true 120 "服务端发现最后一台出声设备掉线,组暂停" group_paused
  [ "$(log_count "出声设备出册")" -gt "$left" ] || fail "组暂停了,但服务端日志里没有「出声设备出册」"

  local before; before=$(joins)
  gate "$OUT" heal
  until_true 30 "OUT 重新入册" joined_since "$before"
  sleep 5
  silent "$OUT" || fail "OUT 重连后按旧状态自己放了起来"
  echo "  ✓ OUT 重连后照组状态停着"

  click "$PEER" "$(play_key "$PEER")"
  until_true 10 "PEER 按 ⏯ 后组在放" group_playing
  until_true 30 "OUT 跟着出声" playing "$OUT"
}

server_restart() {
  : "${RESTART_SERVER:?server-restart 要 RESTART_SERVER:重启服务端的命令}"
  echo "== server-restart:$PEER 只让 $OUT 出声,重启服务端"
  form_group
  local version before; version=$(group_version); before=$(joins)
  bash -c "$RESTART_SERVER"
  until_true 60 "两台重新入册" joined_since $((before + 1))
  [ "$(group_version)" -ge "$version" ] || fail "重启后组版本回退了($version → $(group_version))"
  echo "  ✓ 组状态还在库里,版本没回退"
  until_true 30 "OUT 照状态接着出声" playing "$OUT"
}

restarts() {
  : "${RESTART_OUT:?}" "${RESTART_PEER:?}"
  local rounds=${ROUNDS:-5} j0 t0
  j0=$(joins); t0=$(throttles)
  echo "== restarts:两台一起重启 $rounds 次"
  for r in $(seq "$rounds"); do
    bash -c "$RESTART_OUT" & bash -c "$RESTART_PEER" & wait
    echo "  第 $r 轮已拉起"
  done
  sleep 10
  local joined=$(( $(joins) - j0 )) throttled=$(( $(throttles) - t0 ))
  echo "  入册 $joined 次,限流 $throttled 次"
  [ "$throttled" -eq 0 ] || fail "有设备被限流"
  [ "$joined" -ge $((2 * rounds)) ] || fail "入册次数不够,有设备没连上"
  echo "  ✓ 没有任何一台被限流,每次重启都重新入册"
}

# 半路失败也要把闸放开,不然下一次跑时那台设备一直连不上,报的却是别的错。
trap 'gate "$OUT" heal 2>/dev/null || true; gate "$PEER" heal 2>/dev/null || true; rest' EXIT

case "${1:-}" in
  link-loss) : "${OUT:?}" "${PEER:?}"; link_loss ;;
  last-output) : "${OUT:?}" "${PEER:?}"; last_output ;;
  server-restart) : "${OUT:?}" "${PEER:?}"; server_restart ;;
  restarts) : "${OUT:?}" "${PEER:?}"; restarts ;;
  *) sed -n '2,36p' "$0"; exit 2 ;;
esac
echo "通过"
