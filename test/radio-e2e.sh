#!/usr/bin/env bash
# 端到端:电台(#159)。从用户会点的入口开电台,断言起播、续取,以及续进来的歌都没听过。
#
#   test/radio-e2e.sh fm      Music 页点「电台」分区 → 私人 FM 起播
#   test/radio-e2e.sh heart   控制条抽屉点「从这首开电台」→ 心动模式起播(要已经在放一首)
#
# 起播之后:私人 FM 连按「下一首」直到队列剩最后一首;心动一批上百首,先从队列页点到
# 队尾那首。两种模式都是剩最后一首时电台续一批。
#
# 驱动走应用内嵌的 MCP(形状照 test/pick-e2e.sh),断言走数据库:
#   - play_events 多了一行:电台那一批真的起播了;
#   - 本机队列最新一版的条目数涨了:续取发生了,新的一版发布到了服务端;
#   - 那一版里的每一首(起播那批加续进来的),开电台之前都不在这个账号的播放历史里。
#
# 前提:应用起着(桌面 just desktop-dev,安卓 just mcp-android)、已登录且网易云已绑,
# just server-dev、新版 bang-dream(带 GetPersonalFm / GetIntelligenceList)与 osmosis-pg 在跑。
set -euo pipefail

MODE="${1:?用法: $0 fm|heart}"
PORT="${PORT:-8091}"
PG_CONTAINER="${PG_CONTAINER:-osmosis-pg}"
# 最多按几下「下一首」等续取。私人 FM 一批约 3 首,两三下就该续。
MAX_NEXT="${MAX_NEXT:-8}"

case "$MODE" in
  fm|heart) ;;
  *) echo "用法: $0 fm|heart" >&2; exit 2 ;;
esac

call() {
  curl -s -X POST "http://127.0.0.1:${PORT}/mcp" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}" \
  | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]["content"][0]; print(r.get("text",""))'
}

# 从 from 往下找 id 的元素,第 n 个(缺省第一个;-1 是最后一个)。找不到给空串。
nth() {
  call query_element_descendants "{\"elementHandle\":$1,\"findAll\":true,\"queryStack\":[{\"matchElementId\":\"$2\"}]}" \
  | python3 -c "
import json, sys
hs = json.load(sys.stdin).get('elementHandles') or []
n = ${3:-0}
print(json.dumps(hs[n]) if -len(hs) <= n < len(hs) else '')
"
}

# 某个 id 的元素里,无障碍标签是 label 的那一个。按名字找而不是按位置:
# 分区条插进一项就全体错位(#159),抽屉的行也随功能增减。
labelled() {
  local hs
  hs=$(call query_element_descendants "{\"elementHandle\":$root,\"findAll\":true,\"queryStack\":[{\"matchElementId\":\"$1\"}]}")
  for h in $(echo "$hs" | python3 -c 'import json,sys; [print(json.dumps(h, separators=(",",":"))) for h in json.load(sys.stdin).get("elementHandles") or []]'); do
    if [ "$(call get_element_properties "{\"elementHandle\":$h}" \
      | python3 -c 'import json,sys; print(json.load(sys.stdin).get("accessibleLabel") or "")')" = "$2" ]; then
      echo "$h"; return
    fi
  done
}

must() {
  [ -n "$1" ] || { echo "找不到 $2 —— 页面不对,或者这个构建没有它" >&2; exit 1; }
}

# 无障碍动作不过命中测试,不必量坐标。
act() {
  call invoke_accessibility_action "{\"elementHandle\":$1,\"action\":\"Default_\"}" >/dev/null
}

press() {
  local h; h=$(labelled "$1" "$2")
  must "$h" "「$2」"
  act "$h"
}

sql() {
  docker exec "$PG_CONTAINER" psql -U slint -d osmosis -tAc "$1" | tr -d '[:space:]'
}
lines() {
  docker exec "$PG_CONTAINER" psql -U slint -d osmosis -tAc "$1" | sed 's/^ *//;s/ *$//' | grep -v '^$' || true
}
played() { sql "select count(*) from play_events;"; }
# 最近一次起播的账号就是界面上登着的那个。
account() { sql "select account_id from play_events order by id desc limit 1;"; }
# 最近更新的那个队列:「id:版本:条目数」。
queue_row() {
  sql "select q.id || ':' || q.revision || ':' || (select count(*) from play_queue_entries e
         where e.queue_id = q.id and e.revision = q.revision)
       from play_queues q order by q.updated_at desc limit 1;"
}

win=$(call list_windows '{}' | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["windowHandles"][0]))')
root=$(call get_window_properties "{\"windowHandle\":$win}" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["rootElementHandle"]))')

# 收起播放页,回到音乐页。
for _ in 1 2 3; do
  [ -n "$(nth "$root" "PlayPage::queue-entry-touch")" ] || break
  call dispatch_key_event "{\"windowHandle\":$win,\"text\":\"\\u001b\"}" >/dev/null
  sleep 1
done
music=$(nth "$root" "NavItem::touch" 1)
must "$music" "音乐入口"
call click_element "{\"elementHandle\":$music}" >/dev/null
sleep 1

# 开电台之前这个账号听过的全部歌。私人 FM 之前没放过歌的话,起播之后再认账号 ——
# 那时历史里只多了电台自己放的那一首,它本来就不该在开电台之前的历史里。
history_before=$(mktemp)
trap 'rm -f "$history_before"' EXIT
snapshot() {
  lines "select distinct platform || ':' || track_id from play_events where account_id = $1;" \
    | sort > "$history_before"
}
who=$(account)
[ -z "$who" ] || snapshot "$who"
played_before=$(played)
queue_before=$(queue_row)

if [ "$MODE" = fm ]; then
  item=$(labelled "MusicRail::item-touch" "电台")
  [ -n "$item" ] || item=$(labelled "MusicBar::item-touch" "电台")
  must "$item" "「电台」分区"
  act "$item"
else
  [ -n "$who" ] || { echo "heart: 要先在放一首歌(它是种子)" >&2; exit 1; }
  press "RoundControl::touch" "更多"
  sleep 1
  press "DrawerRow::touch" "从这首开电台"
fi
echo "$MODE: 开了电台"

for _ in $(seq 1 60); do
  sleep 1
  [ "$(played)" -gt "$played_before" ] && break
done
[ "$(played)" -gt "$played_before" ] || { echo "$MODE: 失败 —— 60 秒没起播" >&2; exit 1; }
if [ -z "$who" ]; then
  who=$(account)
  snapshot "$who"
  # 刚起播的那一首是电台放的,不算开电台之前的历史
  first=$(lines "select platform || ':' || track_id from play_events where account_id = $who order by id desc limit 1;")
  grep -vxF "$first" "$history_before" > "$history_before.x" || true
  mv "$history_before.x" "$history_before"
fi
echo "  起播了(play_events +$(( $(played) - played_before )))"

# 等起播那一批发布到服务端,记下它的条目数:续取之后必须比它多。
for _ in $(seq 1 20); do
  [ "$(queue_row)" != "$queue_before" ] && break
  sleep 1
done
started=$(queue_row)
[ "$started" != "$queue_before" ] || { echo "$MODE: 失败 —— 电台那一批没发布成队列" >&2; exit 1; }
queue_id=${started%%:*}
start_count=${started##*:}
echo "  电台那一批 $start_count 首"

# 队列涨过 start_count 就是续上了。
grown=""
wait_growth() {
  for _ in $(seq 1 "$1"); do
    sleep 1
    local now; now=$(queue_row)
    if [ "${now%%:*}" = "$queue_id" ] && [ "${now##*:}" -gt "$start_count" ]; then
      grown=$now; return
    fi
  done
}

# 心动一批上百首:展开播放页、打开队列页,一屏一屏往下点最后那一行,直到放到队尾那首。
skip_to_tail() {
  local cover entry page last
  cover=$(nth "$root" "PlayerBar::cover-touch")
  must "$cover" "控制条封面"
  call click_element "{\"elementHandle\":$cover}" >/dev/null
  sleep 2
  entry=$(nth "$root" "PlayPage::queue-entry-touch")
  must "$entry" "播放页的队列入口"
  call click_element "{\"elementHandle\":$entry}" >/dev/null
  sleep 2
  page=$(call query_element_descendants "{\"elementHandle\":$root,\"findAll\":false,\"queryStack\":[{\"matchElementTypeName\":\"QueuePage\"}]}" \
    | python3 -c 'import json,sys; hs=json.load(sys.stdin).get("elementHandles") or []; print(json.dumps(hs[0]) if hs else "")')
  must "$page" "队列页"
  # 列表是虚拟化的,树里只有滑进可见区的那几行。点可见的最后一行,列表跟着滚,
  # 下一轮就能看见更后面的 —— 直到放的就是队尾那首,续取随之发生。
  for _ in $(seq 1 100); do
    last=$(nth "$page" "TrackList::touch" -1)
    must "$last" "队列页的行"
    call click_element "{\"elementHandle\":$last}" >/dev/null
    wait_growth 3
    [ -z "$grown" ] || return 0
  done
  echo "heart: 失败 —— 点到队尾也没续" >&2
  exit 1
}

if [ "$MODE" = heart ] && [ "$start_count" -gt 2 ]; then
  skip_to_tail
fi
for n in $(seq 1 "$MAX_NEXT"); do
  [ -z "$grown" ] || break
  press "RoundControl::touch" "下一首"
  echo "  按了第 $n 下「下一首」"
  # 续取由每秒一趟的轮询发起,只在放着的时候走;等它问完平台、发布新的一版
  wait_growth 15
done
[ -n "$grown" ] || { echo "$MODE: 失败 —— 按到第 $MAX_NEXT 下队列也没续" >&2; exit 1; }
echo "  续上了:$start_count → ${grown##*:} 首"

revision=$(echo "$grown" | cut -d: -f2)
heard=$(lines "select platform || ':' || track_id from play_queue_entries
               where queue_id = $queue_id and revision = $revision;" \
  | sort | comm -12 - "$history_before")
if [ -n "$heard" ]; then
  echo "$MODE: 失败 —— 这些歌开电台之前就听过:" >&2
  echo "$heard" >&2
  exit 1
fi
echo "$MODE: 通过"
