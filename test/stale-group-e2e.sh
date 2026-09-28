#!/usr/bin/env bash
# 端到端:挂在旧组上的两种卡死(#165)。开发库里造一个从不在线的幽灵设备,重启被测应用让它
# 入册时拿到那份组状态,再从用户会点的入口走一遍:
#
#   test/stale-group-e2e.sh dead   本机是组成员,出声设备只有幽灵。在音乐页点一首:
#                                  本机退组、本机出声,库里组成员已经没有本机。
#   test/stale-group-e2e.sh ghost  本机独奏,组的出声设备里挂着幽灵。个人页点「加入 <另一台>」:
#                                  建组成功(不再 400「有设备不在线」),幽灵被剔出出声设备。
#
# 环境:
#   PORT       被测那台的 MCP 端口:8091 桌面,8090 安卓
#   ME         被测那台的设备 id(桌面在 ~/.local/state/osmosis-dev/device;安卓看服务端日志的「设备入册」)
#   RESTART    重启被测应用的命令;跑完应用可以还没起来,脚本自己等 MCP 应答
#   DESK_LOG   桌面被测 dead 时:应用日志(RUST_LOG=info,ui=debug),用它判出声
#   SERVER_LOG ghost:服务端日志,确认这一趟没有 400
#
# 前提:just server-dev 在跑,被测那台已登录,组里放过歌(组那一行有 queue_id);ghost 还要另一台
# 在线(它是被加入的那台)。断言走数据库、dumpsys audio 与应用日志,不看截图。
set -euo pipefail

MODE="${1:?用法: $0 dead|ghost}"
PORT="${PORT:?要 PORT}"
ME="${ME:?要 ME:被测那台的设备 id}"
RESTART="${RESTART:?要 RESTART:重启被测应用的命令}"
PG_CONTAINER="${PG_CONTAINER:-osmosis-pg}"
GHOST="ghost-165"
ANDROID_PORT="${ANDROID_MCP_PORT:-8090}"
case "$MODE" in
  dead) [ "$PORT" = "$ANDROID_PORT" ] || : "${DESK_LOG:?桌面被测时要 DESK_LOG}" ;;
  ghost) : "${SERVER_LOG:?ghost 要 SERVER_LOG}" ;;
  *) echo "用法: $0 dead|ghost" >&2; exit 2 ;;
esac

call() {
  curl -s -X POST "http://127.0.0.1:${PORT}/mcp" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}" \
  | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]["content"][0]; print(r.get("text",""))'
}
sql() { docker exec "$PG_CONTAINER" psql -U slint -d osmosis -tAc "$1" | tr -d '[:space:]'; }
fail() { echo "$MODE: 失败 —— $1" >&2; exit 1; }

# 那个账号的组:取最近改过的那一行(开发库里在用的就一个账号)。
account=$(sql "select account_id from play_groups order by version desc limit 1;")
[ -n "$account" ] || fail "库里没有组 —— 先在应用里建过一次组"
[ -n "$(sql "select 1 from play_groups where account_id = $account and queue_id is not null;")" ] \
  || fail "组里没有歌(queue_id 空):先在组里放过一首"
members() { sql "select array_to_string(members, ',') from play_groups where account_id = $account;"; }
outputs() { sql "select array_to_string(outputs, ',') from play_groups where account_id = $account;"; }

# 造幽灵。改库不广播,所以接着重启被测应用:入册时服务端推一份当前状态给它。
if [ "$MODE" = dead ]; then
  who="array['$ME', '$GHOST']"
else
  who="array['$GHOST']"
fi
sql "update play_groups set members = $who, outputs = array['$GHOST'],
       playing = false, version = version + 1 where account_id = $account;" >/dev/null
echo "$MODE: 造好幽灵 —— 成员 $(members),出声 $(outputs)"

bash -c "$RESTART" >/dev/null 2>&1 || true
sleep 3
win=""
for _ in $(seq 1 60); do
  win=$(call list_windows '{}' 2>/dev/null \
    | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["windowHandles"][0]))' 2>/dev/null) || win=""
  [ -z "$win" ] || break
  sleep 2
done
[ -n "$win" ] || fail "被测应用两分钟没起来"
sleep 8  # 入册、拿到组状态
root=$(call get_window_properties "{\"windowHandle\":$win}" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["rootElementHandle"]))')

# 按元素 id 找,第 n 个(-1 是最后一个)。
nth() {
  call query_element_descendants "{\"elementHandle\":$root,\"findAll\":true,\"queryStack\":[{\"matchElementId\":\"$1\"}]}" \
  | python3 -c "
import json, sys
hs = json.load(sys.stdin).get('elementHandles') or []
n = ${2:-0}
print(json.dumps(hs[n]) if -len(hs) <= n < len(hs) else '')
"
}
# 某个 id 的元素里,无障碍标签以 $2 开头、又不等于 $3 的第一个。
labelled() {
  local hs h label
  hs=$(call query_element_descendants "{\"elementHandle\":$root,\"findAll\":true,\"queryStack\":[{\"matchElementId\":\"$1\"}]}")
  for h in $(echo "$hs" | python3 -c 'import json,sys; [print(json.dumps(h, separators=(",",":"))) for h in json.load(sys.stdin).get("elementHandles") or []]'); do
    label=$(call get_element_properties "{\"elementHandle\":$h}" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("accessibleLabel") or "")')
    case "$label" in "$2"*) [ "$label" = "${3:-}" ] || { echo "$h"; return; } ;; esac
  done
}
act() { call invoke_accessibility_action "{\"elementHandle\":$1,\"action\":\"Default_\"}" >/dev/null; }
# 列表行没挂无障碍默认动作:桌面走 click_element,真机走 adb 的真实触摸(见 test/radio-e2e.sh)。
tap() {
  if [ "$PORT" = "$ANDROID_PORT" ]; then
    local k xy
    k=$(call get_window_properties "{\"windowHandle\":$win}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["scaleFactor"])')
    xy=$(call get_element_properties "{\"elementHandle\":$1}" | python3 -c "
import json, sys
p = json.load(sys.stdin); a = p['absolutePosition']; s = p['size']
print(int((a.get('x', 0) + s['width'] / 2) * $k), int((a.get('y', 0) + s['height'] / 2) * $k))
")
    # shellcheck disable=SC2086
    ${ADB:-adb} shell input tap $xy
  else
    call click_element "{\"elementHandle\":$1}" >/dev/null
  fi
}
# 本机这个应用此刻在不在出声(形状照 test/link-loss-e2e.sh 的 playing)。
sounding() {
  if [ "$PORT" = "$ANDROID_PORT" ]; then
    local pid
    pid=$(${ADB:-adb} shell pidof io.github.osmosis | tr -d '\r')
    [ -n "$pid" ] && ${ADB:-adb} shell dumpsys audio | grep -qE "u/pid:[0-9]+/$pid state:started"
  else
    local a b
    a=$(grep "自动续播轮询" "$DESK_LOG" | tail -1)
    sleep 2
    b=$(grep "自动续播轮询" "$DESK_LOG" | tail -1)
    [[ "$b" == *"放空 false"* && "${a#*位置 }" != "${b#*位置 }" ]]
  fi
}
# 服务端回过几次 /group/outputs 的 400。
refusals() { grep "/group/outputs" "$SERVER_LOG" | grep -c "status=400" || true; }

if [ "$MODE" = dead ]; then
  music=$(nth "NavItem::touch" 1)
  [ -n "$music" ] || fail "找不到音乐入口"
  act "$music"
  sleep 3
  row=$(nth "TrackList::touch" -1)
  [ -n "$row" ] || fail "音乐页的列表是空的,没歌可点"
  tap "$row"
  echo "  点了一首"
  left=""
  for _ in $(seq 1 20); do
    sleep 1
    case ",$(members)," in *",$ME,"*) ;; *) left=yes; break ;; esac
  done
  [ -n "$left" ] || fail "20 秒后本机还在组成员里:$(members)"
  echo "  库里组成员已无本机(成员 $(members))"
  ok=""
  for _ in $(seq 1 10); do
    if sounding; then ok=yes; break; fi
    sleep 2
  done
  [ -n "$ok" ] || fail "退了组,本机没出声"
  echo "  本机在出声"
else
  before=$(refusals)
  tab=$(labelled "RoundControl::touch" "个人")
  [ -n "$tab" ] || fail "找不到「个人」"
  act "$tab"
  sleep 2
  join=$(labelled "MemberToggle::touch" "加入 " "加入 本机")
  [ -n "$join" ] || fail "个人页上没有「加入 <另一台>」:另一台不在线?"
  act "$join"
  joined=""
  for _ in $(seq 1 10); do
    sleep 1
    case ",$(outputs)," in *",$GHOST,"*|,,) ;; *) joined=yes; break ;; esac
  done
  [ "$(refusals)" = "$before" ] || fail "服务端又回了 400"
  [ -n "$joined" ] || fail "10 秒后出声设备还是 $(outputs)"
  case ",$(members)," in *",$ME,"*) ;; *) fail "本机没成为成员:$(members)" ;; esac
  echo "  建组成功:成员 $(members),出声 $(outputs)"
fi
echo "$MODE: 通过"
