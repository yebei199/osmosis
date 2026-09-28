#!/usr/bin/env bash
# 端到端:电台(#159)。从用户会点的入口开电台,断言起播、续取,以及续进来的歌都没听过。
#
#   test/radio-e2e.sh fm      Music 页点「电台」分区 → 私人 FM 起播
#   test/radio-e2e.sh heart   控制条抽屉点「从这首开电台」→ 心动模式起播(要已经在放一首)
#   test/radio-e2e.sh filter  私人 FM 起播后在电台区选 CHIPS(默认「有歌词」)再点一首,
#                             续进来的每首都得过得了这些筛选(#166)
#   test/radio-e2e.sh dry     同上,但 CHIPS 选得窄到续不出:界面要出「按当前筛选暂时找不到新歌」
#
# 起播之后:私人 FM 连按「下一首」直到队列剩最后一首;心动一批上百首,先从队列页点到
# 队尾那首。两种模式都是剩最后一首时电台续一批。
#
# 驱动走应用内嵌的 MCP(形状照 test/pick-e2e.sh),断言走数据库:
#   - play_events 多了一行:电台那一批真的起播了;
#   - 本机队列最新一版的条目数涨了:续取发生了,新的一版发布到了服务端;
#   - 那一版里的每一首(起播那批加续进来的),开电台之前都不在这个账号的播放历史里;
#   - filter:续进来的每一首的 platform_tracks.lyric_kind 是 lyric / translated
#     (只认「有歌词」;CHIPS 换成别的,这一条换成人工核对,脚本只报续进来的歌)。
#   - dry:提示横幅出现,队列没涨。CHIPS 不够窄、居然续上了,退 3(不算过也不算挂)。
#
# 前提:应用起着(桌面 just desktop-dev,安卓 just mcp-android)、已登录且网易云已绑,
# just server-dev、新版 bang-dream(带 GetPersonalFm / GetIntelligenceList)与 osmosis-pg 在跑。
set -euo pipefail

MODE="${1:?用法: $0 fm|heart}"
PORT="${PORT:-8091}"
PG_CONTAINER="${PG_CONTAINER:-osmosis-pg}"
# 服务端连的库。开发库被别的分支迁移到更新的版本时,本分支的 server 要连一份副本。
PG_DB="${PG_DB:-osmosis}"
# 最多按几下「下一首」等续取。私人 FM 一批约 3 首,两三下就该续。
MAX_NEXT="${MAX_NEXT:-8}"

# filter / dry 在电台区选哪几个 chip,逗号分隔,按 chip 文字的前缀认(chip 上还挂着数目)。
case "$MODE" in
  fm|heart) ;;
  filter) CHIPS="${CHIPS:-有歌词}" ;;
  dry) CHIPS="${CHIPS:-无歌词,5 分钟以上}" ;;
  *) echo "用法: $0 fm|heart|filter|dry" >&2; exit 2 ;;
esac
DRY_NOTICE="按当前筛选暂时找不到新歌"

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

# 点一个元素。真机上走 adb 的真实触摸:MCP 的 click_element 在真机上点不开控制条封面
# (2026-09-28 小米 13 实测,adb input tap 同一点就开了)。
tap() {
  if [ "$PORT" = "${ANDROID_MCP_PORT:-8090}" ]; then
    local k xy
    k=$(call get_window_properties "{\"windowHandle\":$win}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["scaleFactor"])')
    xy=$(call get_element_properties "{\"elementHandle\":$1}" | python3 -c "
import json, sys
p = json.load(sys.stdin); a = p['absolutePosition']; s = p['size']
print(int((a.get('x', 0) + s['width'] / 2) * $k), int((a.get('y', 0) + s['height'] / 2) * $k))
")
    ${ADB:-adb} shell input tap $xy
  else
    call click_element "{\"elementHandle\":$1}" >/dev/null
  fi
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
  docker exec "$PG_CONTAINER" psql -U slint -d "$PG_DB" -tAc "$1" | tr -d '[:space:]'
}
lines() {
  docker exec "$PG_CONTAINER" psql -U slint -d "$PG_DB" -tAc "$1" | sed 's/^ *//;s/ *$//' | grep -v '^$' || true
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
# 安卓上 Esc 收不起覆层(队列页、播放页),要按系统返回键。
for _ in 1 2 3 4; do
  [ -n "$(nth "$root" "PlayPage::queue-entry-touch")" ] || break
  if [ "$PORT" = "${ANDROID_MCP_PORT:-8090}" ]; then
    ${ADB:-adb} shell input keyevent KEYCODE_BACK
  else
    call dispatch_key_event "{\"windowHandle\":$win,\"text\":\"\\u001b\"}" >/dev/null
  fi
  sleep 1.5
done
[ -z "$(nth "$root" "PlayPage::queue-entry-touch")" ] || { echo "收不起播放页" >&2; exit 1; }
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

if [ "$MODE" != heart ]; then
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

# 电台区选 chip 再点列表第一行(#166)。chip 的模型每按一次就重建,句柄跟着失效,每次重取。
chip_named() {
  local hs h label
  hs=$(call query_element_descendants "{\"elementHandle\":$root,\"findAll\":true,\"queryStack\":[{\"matchElementId\":\"FacetBar::chip-pill\"}]}")
  for h in $(echo "$hs" | python3 -c 'import json,sys; [print(json.dumps(h, separators=(",",":"))) for h in json.load(sys.stdin).get("elementHandles") or []]'); do
    label=$(call get_element_properties "{\"elementHandle\":$h}" \
      | python3 -c 'import json,sys; print(json.load(sys.stdin).get("accessibleLabel") or "")')
    case "$label" in "$1 "*) echo "$h"; return ;; esac
  done
}
if [ "$MODE" = filter ] || [ "$MODE" = dry ]; then
  toggle=$(nth "$root" "FacetBar::filter-toggle")
  must "$toggle" "筛选开关"
  act "$toggle"
  sleep 1
  IFS=, read -ra wanted <<< "$CHIPS"
  for want in "${wanted[@]}"; do
    chip=$(chip_named "$want")
    [ -n "$chip" ] || { echo "$MODE: 失败 —— 电台区这一批没有「$want」这个 chip(换 CHIPS 或重开电台再试)" >&2; exit 1; }
    act "$chip"
    sleep 1
  done
  row=$(nth "$root" "TrackList::touch")
  [ -n "$row" ] || { echo "$MODE: 失败 —— 选完「$CHIPS」列表空了,没歌可点(换 CHIPS 再试)" >&2; exit 1; }
  act "$row"
  for _ in $(seq 1 20); do
    sleep 1
    [ "$(queue_row)" != "$started" ] && break
  done
  started=$(queue_row)
  queue_id=${started%%:*}
  start_count=${started##*:}
  echo "  选「$CHIPS」点了一首:筛过的那一批 $start_count 首"
fi

# 横幅此刻的文字,没有横幅是空串。
banner() {
  local b
  b=$(nth "$root" "MainWindow::banner")
  [ -n "$b" ] || return 0
  call query_element_descendants "{\"elementHandle\":$b,\"findAll\":true,\"queryStack\":[{\"matchElementTypeName\":\"Text\"}]}" \
    | python3 -c 'import json,sys; hs=json.load(sys.stdin).get("elementHandles") or []; print(json.dumps(hs[0]) if hs else "")' \
    | { read -r t; [ -z "$t" ] || call get_element_properties "{\"elementHandle\":$t}" \
        | python3 -c 'import json,sys; p=json.load(sys.stdin); print(p.get("accessibleLabel") or p.get("accessibleValue") or "")'; }
}

# 队列涨过 start_count 就是续上了。dry 同时盯着横幅,出了提示就记下。
grown=""
told=""
wait_growth() {
  for _ in $(seq 1 "$1"); do
    sleep 1
    if [ "$MODE" = dry ] && [ "$(banner)" = "$DRY_NOTICE" ]; then
      told=yes; return
    fi
    local now; now=$(queue_row)
    if [ "${now%%:*}" = "$queue_id" ] && [ "${now##*:}" -gt "$start_count" ]; then
      grown=$now; return
    fi
  done
}

# 正在放的那一条在这一版里排第几(服务端检查点)。
playing_at() {
  sql "select e.position from play_queue_reports r join play_queue_entries e
         on (e.queue_id, e.revision, e.entry_id) = (r.queue_id, r.applied_revision, r.entry_id)
       where r.queue_id = $queue_id;"
}

# 心动一批上百首,一下一下按「下一首」到队尾要对真账号连取上百次播放源,风控的靶子。
# 改走用户的另一条路:展开播放页、打开队列页,把列表滑到底,点最后一行。
#
# 滑动走 adb 的真实触摸:MCP 的 drag_element 带不动列表(2026-09-28 真机实测)。
# 所以这一步只在真机上走;桌面上心动模式会一直按「下一首」,要按很多下。
# 起点必须落在列表的可视区里 —— 落到可视区外就是点在队列页的空白上,页面收回。
skip_to_tail() {
  local cover entry page flick geo scale row
  local adb=${ADB:-adb}
  cover=$(nth "$root" "PlayerBar::cover-touch")
  must "$cover" "控制条封面"
  tap "$cover"
  # 播放页展开有动画,真机上一两秒到五六秒不等
  for _ in $(seq 1 15); do
    sleep 1
    entry=$(nth "$root" "PlayPage::queue-entry-touch")
    [ -z "$entry" ] || break
  done
  must "$entry" "播放页的队列入口"
  tap "$entry"
  # 队列页横向滑入,动画没落地时元素还在屏外
  sleep 3
  page=$(call query_element_descendants "{\"elementHandle\":$root,\"findAll\":false,\"queryStack\":[{\"matchElementTypeName\":\"QueuePage\"}]}" \
    | python3 -c 'import json,sys; hs=json.load(sys.stdin).get("elementHandles") or []; print(json.dumps(hs[0]) if hs else "")')
  must "$page" "队列页"
  flick=$(call query_element_descendants "{\"elementHandle\":$page,\"findAll\":false,\"queryStack\":[{\"matchElementTypeNameOrBase\":\"Flickable\"}]}" \
    | python3 -c 'import json,sys; hs=json.load(sys.stdin).get("elementHandles") or []; print(json.dumps(hs[0]) if hs else "")')
  must "$flick" "队列页的列表"
  scale=$(call get_window_properties "{\"windowHandle\":$win}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["scaleFactor"])')
  # 可视区的「x 中线 起点y 终点y 下缘y」,起终点换成物理像素给 adb
  geo=$(call get_element_properties "{\"elementHandle\":$flick}" | python3 -c "
import json, sys
p = json.load(sys.stdin); a = p['absolutePosition']; s = p['size']; k = $scale
x, y, h = a.get('x', 0) + s['width'] / 2, a.get('y', 0), s['height']
print(int(x * k), int((y + h * 0.85) * k), int((y + h * 0.1) * k), y + h)
")
  set -- $geo
  for _ in $(seq 1 5); do
    for _ in $(seq 1 30); do
      $adb shell input swipe "$1" "$2" "$1" "$3" 120
    done
    sleep 2
    # 可视区里最靠下、整行都露着的那一行
    row=$(call query_element_descendants "{\"elementHandle\":$flick,\"findAll\":true,\"queryStack\":[{\"matchElementId\":\"TrackList::touch\"}]}" \
      | python3 -c 'import json,sys; [print(json.dumps(h, separators=(",",":"))) for h in json.load(sys.stdin).get("elementHandles") or []]' \
      | while read -r h; do
          call get_element_properties "{\"elementHandle\":$h}" \
            | python3 -c "import json,sys; p=json.load(sys.stdin); y=p['absolutePosition'].get('y',0); print(y, '$h') if y + p['size']['height'] <= $4 else None"
        done | sort -n | tail -1 | cut -d' ' -f2)
    must "$row" "队列页可视区里的行"
    tap "$row"
    sleep 3
    [ "$(playing_at)" = "$((start_count - 1))" ] && return 0
  done
  echo "heart: 失败 —— 滑不到队尾(此刻放到第 $(playing_at) 条,共 $start_count 条)" >&2
  exit 1
}

if [ "$MODE" = heart ] && [ "$start_count" -gt 2 ] && [ "$PORT" = "${ANDROID_MCP_PORT:-8090}" ]; then
  skip_to_tail
  echo "  跳到了队尾第 $start_count 首"
  wait_growth 20
fi
for n in $(seq 1 "$MAX_NEXT"); do
  [ -z "$grown" ] && [ -z "$told" ] || break
  press "RoundControl::touch" "下一首"
  echo "  按了第 $n 下「下一首」"
  # 续取由每秒一趟的轮询发起,只在放着的时候走;等它问完平台、发布新的一版
  wait_growth 15
done
if [ "$MODE" = dry ]; then
  [ -z "$grown" ] || { echo "dry: 不确定 —— 「$CHIPS」不够窄,续上了:$start_count → ${grown##*:} 首(换更窄的 CHIPS 再跑)" >&2; exit 3; }
  [ -n "$told" ] || { echo "dry: 失败 —— 续不出新歌,界面也没出「$DRY_NOTICE」" >&2; exit 1; }
  echo "dry: 通过 —— 界面出了「$DRY_NOTICE」,队列没涨"
  exit 0
fi
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
if [ "$MODE" = filter ]; then
  # 续进来的:这一版里排在筛过那一批之后的
  added=$(lines "select e.platform || ':' || e.track_id || ' ' || coalesce(d.lyric_kind, '不在缓存')
                 from play_queue_entries e
                 left join platform_tracks d on (d.platform, d.track_id) = (e.platform, e.track_id)
                 where e.queue_id = $queue_id and e.revision = $revision and e.position >= $start_count
                 order by e.position;")
  echo "  续进来的:"; echo "$added" | sed 's/^/    /'
  if [ "$CHIPS" = 有歌词 ]; then
    wrong=$(echo "$added" | grep -vE ' (lyric|translated)$' || true)
    [ -z "$wrong" ] || { echo "filter: 失败 —— 这些续进来的不是有歌词:" >&2; echo "$wrong" >&2; exit 1; }
  fi
fi
echo "$MODE: 通过"
