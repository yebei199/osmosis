#!/usr/bin/env bash
# #118 的三条验收,走真界面、看真相源:
#
#   link-loss   CTL 遥控 TGT;掐断 TGT 的信令(HTTP 照通)→ TGT 横幅撤掉、本机点歌落账;
#               恢复信令 → CTL 回到「本机」,TGT 不再挂横幅。
#   claim-fail  掐断 CTL 的信令(它的名册就停在这一刻)→ TGT 下线 → CTL 点 TGT 那颗芯片
#               → 恢复信令 → 接管失败,CTL 回到「本机」,本机点歌落账。
#   restarts    两台一起反复重启应用 → 服务端一次限流都没有,每次都重新入册。
#
# #111 的两条验收(遥控器消失而被控端连接完好):
#
#   vanish      TGT 本机放着歌,CTL 接管它,然后把 CTL 杀掉(安卓 force-stop、ns 实例
#               SIGKILL)→ 租约满之前横幅还在,满了自己撤掉,TGT 一直在出声。
#   blip        CTL 遥控 TGT;掐断 CTL 的信令 CUT 秒(缺省 5)再放开 → CTL 带代次续上,
#               再等过一个租约,TGT 仍被 CTL 遥控,服务端没有清过控制权。
#
# 真相源:play_events 的行数(本机起播必记一行)、服务端日志(入册、限流、租约),安卓另看
# dumpsys audio 里有没有 state:started 的 AudioTrack,ns 实例看播放器自己的位置日志在不在走。都不看截图。
#
# 设备用「种类:参数」写:
#   ns:<目录>       namespace 里的桌面实例,由 test/ns-desktop.sh <目录> 起;目录里有 ns.pid
#                   (就是应用进程)与 gate-desk.pid(ns 里那道 signal-gate)。
#   android:<闸pid> 真机,MCP 走 adb forward 的 8090;闸是宿主上那道 signal-gate 的 pid
#                   (adb reverse tcp:3000 指到它)。
#
# 用法:
#   CTL=ns:/path/a TGT=android:12345 SERVER_LOG=/path/server.log test/link-loss-e2e.sh link-loss
#   CTL=android:12345 TGT=ns:/path/a SERVER_LOG=... RESTART_TGT=<重启 TGT 的命令> test/link-loss-e2e.sh claim-fail
#   CTL=... TGT=... SERVER_LOG=... RESTART_CTL=<命令> RESTART_TGT=<命令> ROUNDS=5 test/link-loss-e2e.sh restarts
#   CTL=... TGT=... SERVER_LOG=... [LEASE=30] test/link-loss-e2e.sh vanish
#   CTL=... TGT=... SERVER_LOG=... [LEASE=30] [CUT=5] test/link-loss-e2e.sh blip
#
# LEASE 要与服务端的 `control::LEASE` 一致;SERVER_LOG 那份 server 要带
# `RUST_LOG=info,server=debug`(入册是 debug 级);ns 实例当 TGT 时要带
# `RUST_LOG=info,ui=debug` 起(`playing` 读播放器的位置日志)。
#
# 前提:两台都登录在同一个账号上(test/mcp-login.sh),连的是 SERVER_LOG 那份 server。
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

controlled() { [ -n "$(handles "$1" MainWindow::controlled)" ]; }
not_controlled() { ! controlled "$1"; }

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

# 本机点一首歌(音乐页 → 列表 → 第一或第二行),断言 play_events 多一行。
play_locally() {
  local dev=$1 before idx
  before=$(plays)
  click "$dev" "$(handles "$dev" NavItem::touch | sed -n 2p)"; sleep 1.5
  click "$dev" "$(handles "$dev" WallView::view-list-btn | head -1)"; sleep 1.5
  # 同一行正在放时再点是多余的点击,不会重新起播 —— 两行轮着点,见 played-e2e.sh。
  for idx in 1 2; do
    # 点一下就起播。别连点两下:2026-09-23 在 ns 桌面实例上实测,同一行连点两下
    # 什么都没放,状态回到「点一首歌开始」(#111 回报里记为待裁决发现)。
    click "$dev" "$(handles "$dev" TrackList::touch | sed -n "${idx}p")"
    for _ in $(seq 20); do
      sleep 1
      if [ "$(plays)" -gt "$before" ]; then
        echo "  ✓ 本机起播落账(play_events $before → $(plays))"
        [ "$(kind "$dev")" = android ] && audio_started
        return 0
      fi
    done
  done
  fail "$dev 本机点歌 40 秒没落账"
}

# 只认本应用那个进程的播放器 —— 别的应用在放也会有 state:started。
audio_started() {
  local pid; pid=$(adb shell pidof io.github.osmosis | tr -d '\r')
  adb shell dumpsys audio | grep -qE "u/pid:[0-9]+/$pid state:started" \
    && echo "  ✓ dumpsys audio 里本应用($pid)的播放器 state:started" \
    || fail "dumpsys audio 里本应用($pid)没有 started 的播放器"
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

# 杀掉应用,模拟崩溃或被系统收走:不给它任何收尾的机会。
kill_app() {
  case "$(kind "$1")" in
    ns) kill -9 "$(cat "$(arg "$1")/ns.pid")" ;;
    android) adb shell am force-stop io.github.osmosis ;;
  esac
}

log_count() { grep -c "$1" "$SERVER_LOG" || true; }
resumed_since() { [ "$(log_count "遥控器租约内续上")" -gt "$1" ]; }

joins() { grep -c "设备入册" "$SERVER_LOG" || true; }
joined_since() { [ "$(joins)" -gt "$1" ]; }
throttles() { grep -c "限流挡下一条请求" "$SERVER_LOG" || true; }

link_loss() {
  local tgt_name; tgt_name=$(name_of "$TGT")
  echo "== link-loss:$CTL 遥控 $TGT($tgt_name),掐 TGT 的信令"
  open_profile "$CTL"
  click "$CTL" "$(chip "$CTL" "$tgt_name")"
  until_true 10 "TGT 挂上「正被遥控」" controlled "$TGT"

  gate "$TGT" cut
  until_true 15 "TGT 断线后横幅撤掉" not_controlled "$TGT"
  play_locally "$TGT"

  local before; before=$(joins)
  gate "$TGT" heal
  until_true 30 "TGT 重新入册" joined_since "$before"
  open_profile "$CTL"
  until_true 20 "CTL 回到本机输出" chip_checked "$CTL" 本机
  controlled "$TGT" && fail "TGT 恢复后又挂上了横幅"
  echo "  ✓ TGT 恢复后没有被锁回去"
}

claim_fail() {
  : "${RESTART_TGT:?claim-fail 要 RESTART_TGT:把 TGT 重新拉起来的命令,收尾用}"
  local tgt_name; tgt_name=$(name_of "$TGT")
  echo "== claim-fail:掐 $CTL 的信令,$TGT($tgt_name)下线,再去接管它"
  open_profile "$CTL"
  chip "$CTL" "$tgt_name" >/dev/null || fail "CTL 名册里没有 $tgt_name"
  gate "$CTL" cut
  sleep 2
  case "$(kind "$TGT")" in
    ns) kill "$(cat "$(arg "$TGT")/ns.pid")" ;;
    android) adb shell am force-stop io.github.osmosis ;;
  esac
  sleep 2
  click "$CTL" "$(chip "$CTL" "$tgt_name")"
  gate "$CTL" heal
  until_true 30 "CTL 接管失败后回到本机输出" chip_checked "$CTL" 本机
  play_locally "$CTL"
  echo "  (把 TGT 拉回来)"; bash -c "$RESTART_TGT"
}

restarts() {
  : "${RESTART_CTL:?}" "${RESTART_TGT:?}"
  local rounds=${ROUNDS:-5} j0 t0
  j0=$(joins); t0=$(throttles)
  echo "== restarts:两台一起重启 $rounds 次"
  for r in $(seq "$rounds"); do
    bash -c "$RESTART_CTL" & bash -c "$RESTART_TGT" & wait
    echo "  第 $r 轮已拉起"
  done
  sleep 10
  local joined=$(( $(joins) - j0 )) throttled=$(( $(throttles) - t0 ))
  echo "  入册 $joined 次,限流 $throttled 次"
  [ "$throttled" -eq 0 ] || fail "有设备被限流"
  [ "$joined" -ge $((2 * rounds)) ] || fail "入册次数不够,有设备没连上"
  echo "  ✓ 没有任何一台被限流,每次重启都重新入册"
}

vanish() {
  local lease=${LEASE:-30} tgt_name t0 expired
  tgt_name=$(name_of "$TGT")
  echo "== vanish:$CTL 遥控正在放歌的 $TGT($tgt_name),然后杀掉 $CTL"
  play_locally "$TGT"
  open_profile "$CTL"
  click "$CTL" "$(chip "$CTL" "$tgt_name")"
  until_true 10 "TGT 挂上「正被遥控」" controlled "$TGT"
  playing "$TGT" || fail "接管之后 TGT 不出声了"

  expired=$(log_count "遥控器下线满租约")
  kill_app "$CTL"
  t0=$SECONDS
  # 早于租约撤掉也算错:那说明清锁的不是租约,短暂断网同样会被它清掉。
  sleep $((lease - 10))
  controlled "$TGT" || fail "CTL 消失 $((SECONDS - t0)) 秒、租约还没满,横幅就撤了"
  echo "  ✓ 租约满之前($((SECONDS - t0)) 秒)横幅还在"
  until_true 30 "满租约后 TGT 横幅自己撤掉" not_controlled "$TGT"
  echo "  (CTL 消失到横幅撤掉:$((SECONDS - t0)) 秒,租约 $lease 秒)"
  [ "$(log_count "遥控器下线满租约")" -gt "$expired" ] \
    || fail "横幅撤了,但服务端没有按租约清控制权"
  echo "  ✓ 服务端日志:遥控器下线满租约,清掉控制权"
  playing "$TGT" || fail "横幅撤掉时 TGT 的播放也停了"
  echo "  ✓ TGT 一直在出声"
}

blip() {
  local lease=${LEASE:-30} cut=${CUT:-5} tgt_name j0 r0 e0
  tgt_name=$(name_of "$TGT")
  echo "== blip:$CTL 遥控 $TGT($tgt_name),掐 CTL 的信令 $cut 秒再放开"
  open_profile "$CTL"
  click "$CTL" "$(chip "$CTL" "$tgt_name")"
  until_true 10 "TGT 挂上「正被遥控」" controlled "$TGT"

  j0=$(joins); r0=$(log_count "遥控器租约内续上"); e0=$(log_count "遥控器下线满租约")
  gate "$CTL" cut
  sleep "$cut"
  gate "$CTL" heal
  until_true 30 "CTL 重新入册" joined_since "$j0"
  until_true 15 "服务端认到 CTL 带代次续上" resumed_since "$r0"

  echo "  (再等过一个租约:$((lease + 10)) 秒)"
  sleep $((lease + 10))
  controlled "$TGT" || fail "短暂断网被当成离线,TGT 的横幅被清掉了"
  open_profile "$CTL"
  chip_checked "$CTL" "$tgt_name" || fail "CTL 不再指着 TGT"
  [ "$(log_count "遥控器下线满租约")" -eq "$e0" ] \
    || fail "服务端按租约清过控制权"
  echo "  ✓ 过了一个租约,TGT 仍被 CTL 遥控,服务端没清过控制权"
}

# 半路失败也要把闸放开,不然下一次跑时那台设备一直连不上,报的却是别的错。
trap 'gate "$CTL" heal 2>/dev/null || true; gate "$TGT" heal 2>/dev/null || true' EXIT

case "${1:-}" in
  link-loss) : "${CTL:?}" "${TGT:?}"; link_loss ;;
  claim-fail) : "${CTL:?}" "${TGT:?}"; claim_fail ;;
  restarts) : "${CTL:?}" "${TGT:?}"; restarts ;;
  vanish) : "${CTL:?}" "${TGT:?}"; vanish ;;
  blip) : "${CTL:?}" "${TGT:?}"; blip ;;
  *) sed -n '2,37p' "$0"; exit 2 ;;
esac
echo "通过"
