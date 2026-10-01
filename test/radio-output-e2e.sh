#!/usr/bin/env bash
# #174:真实电台起播后点 pc1 输出。只在单账号的隔离开发库造损坏引用。
# direct:切输出请求自己恢复;recovered:重入册先恢复空组,再开电台并切输出。
# 必填 ACCOUNT_ID/ME/TARGET/TARGET_PORT/TARGET_LOG/PG_CONTAINER;recovered 还要 RESTART。
# 桌面源另设 SOURCE_LOG;安卓源设 ANDROID_SERIAL。运行前应用已登录。
set -euo pipefail
MODE="${1:?用法: $0 direct|recovered}"
PORT="${PORT:-8091}"
: "${ACCOUNT_ID:?要隔离测试账号 ID}" "${ME:?要源设备 ID}" "${TARGET:?要 pc1 设备 ID}"
: "${TARGET_PORT:?要 pc1 MCP 端口}" "${TARGET_LOG:?要 pc1 播放器日志}"
: "${PG_CONTAINER:?要本轮独立 Postgres 容器}"
[[ "$PG_CONTAINER" == osmosis-174-* && "$ACCOUNT_ID" =~ ^[0-9]+$ ]] || exit 2
[[ "$ME" =~ ^[A-Za-z0-9_-]+$ && "$TARGET" =~ ^[A-Za-z0-9_-]+$ && "$ME" != "$TARGET" ]] || exit 2
case "$MODE" in
 direct) ;;
 recovered) : "${RESTART:?要重启源实例的命令}" ;;
 *) exit 2 ;;
esac
if [ "$PORT" != 8090 ]; then : "${SOURCE_LOG:?桌面源要 SOURCE_LOG}"; fi

sql() { docker exec "$PG_CONTAINER" psql -U slint -d osmosis -v ON_ERROR_STOP=1 -Atc "$1"; }
[ "$(sql "SELECT count(*) FROM accounts;")" = 1 ] || { echo "拒绝在非单账号测试库造数" >&2; exit 2; }
[ "$(sql "SELECT count(*) FROM accounts WHERE id = $ACCOUNT_ID;")" = 1 ] || exit 2

call() {
 curl --fail --silent --show-error --max-time 10 -X POST "http://127.0.0.1:${CALL_PORT:-$PORT}/mcp" \
  -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}" \
 | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]; assert not r.get("isError"), r; print(r["content"][0]["text"])'
}
refresh() {
 win=$(call list_windows '{}' | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["windowHandles"][0]))')
 root=$(call get_window_properties "{\"windowHandle\":$win}" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["rootElementHandle"]))')
}
nth() {
 call query_element_descendants "{\"elementHandle\":$root,\"findAll\":true,\"queryStack\":[{\"matchElementId\":\"$1\"}]}" \
 | python3 -c "import json,sys; hs=json.load(sys.stdin).get('elementHandles') or []; print(json.dumps(hs[${2:-0}]) if len(hs) > ${2:-0} else '')"
}
labelled() {
 local hs h label
 hs=$(call query_element_descendants "{\"elementHandle\":$root,\"findAll\":true,\"queryStack\":[{\"matchElementId\":\"$1\"}]}")
 for h in $(echo "$hs" | python3 -c 'import json,sys; [print(json.dumps(h,separators=(",",":"))) for h in json.load(sys.stdin).get("elementHandles") or []]'); do
  label=$(call get_element_properties "{\"elementHandle\":$h}" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("accessibleLabel") or "")')
  if [ "$label" = "$2" ]; then echo "$h"; return; fi
 done
}
act() {
 [ -n "$1" ] || { echo "找不到用户入口" >&2; exit 1; }
 call invoke_accessibility_action "{\"elementHandle\":$1,\"action\":\"Default_\"}" >/dev/null
}
profile() {
 refresh
 local h
 h=$(labelled "RoundControl::touch" "个人")
 [ -n "$h" ] || h=$(nth "NavItem::touch" 2)
 [ -n "$h" ] || exit 1
 call click_element "{\"elementHandle\":$h}" >/dev/null
}
radio() {
 PORT="$PORT" PG_CONTAINER="$PG_CONTAINER" ADB="adb -s ${ANDROID_SERIAL:-unused}" bash "$(dirname "$0")/radio-e2e.sh" fm
}
profile
act "$(labelled "OutputChip::touch" "输出到 本机")"
radio
# 要至少十秒的真实本机进度,才能区分接续与从零重播。
for _ in $(seq 1 45); do
 snapshot=$(sql "SELECT q.id || '|' || r.applied_revision || '|' || r.entry_id || '|' || r.position_ms
 FROM play_queues q JOIN play_queue_reports r ON r.queue_id=q.id
 WHERE q.account_id=$ACCOUNT_ID AND q.device_id='$ME' AND r.play_state='playing'
 ORDER BY r.reported_at DESC LIMIT 1;")
 IFS='|' read -r queue revision entry position <<< "$snapshot"
 if [[ "${position:-}" =~ ^[0-9]+$ ]] && [ "$position" -ge 10000 ]; then break; fi
 sleep 1
done
[[ "${position:-}" =~ ^[0-9]+$ && "$position" -ge 10000 ]] || { echo "本机电台没有有效进度" >&2; exit 1; }
# SQL 仅造损坏现场,实际输出选择始终由 MCP 点击。
sql "INSERT INTO play_groups(account_id,version,members,outputs,queue_id,revision,entry_id,playing)
 VALUES($ACCOUNT_ID,1,ARRAY['$TARGET'],ARRAY['$TARGET'],$queue,$revision+1000000,$entry,false)
 ON CONFLICT(account_id) DO UPDATE SET version=play_groups.version+1,members=EXCLUDED.members,
 outputs=EXCLUDED.outputs,queue_id=EXCLUDED.queue_id,revision=EXCLUDED.revision,entry_id=EXCLUDED.entry_id,
 playing=false,position_us=0,anchor_wall_us=0,boundary_wall_us=NULL,alive_wall_us=NULL,play_order='{}';" >/dev/null
if [ "$MODE" = recovered ]; then
 bash -c "$RESTART"
 for _ in $(seq 1 60); do
  if [ "$(sql "SELECT queue_id IS NULL AND revision IS NULL AND entry_id IS NULL FROM play_groups WHERE account_id=$ACCOUNT_ID;")" = t ]; then break; fi
  sleep 1
 done
 [ "$(sql "SELECT queue_id IS NULL AND revision IS NULL AND entry_id IS NULL FROM play_groups WHERE account_id=$ACCOUNT_ID;")" = t ]
 echo "重入册已持久恢复空组"
 refresh
 radio
fi

# 点击前再抓当前曲目与进度,不用开始电台时的旧快照。
snapshot=$(sql "SELECT q.id || '|' || r.applied_revision || '|' || r.entry_id || '|' || r.position_ms
 FROM play_queues q JOIN play_queue_reports r ON r.queue_id=q.id
 WHERE q.account_id=$ACCOUNT_ID AND q.device_id='$ME' AND r.play_state='playing'
 ORDER BY r.reported_at DESC LIMIT 1;")
IFS='|' read -r queue revision entry position <<< "$snapshot"
[[ "$queue" =~ ^[0-9]+$ && "$revision" =~ ^[0-9]+$ && "$entry" =~ ^[0-9]+$ && "$position" =~ ^[0-9]+$ ]]
title=$(sql "SELECT title FROM play_queue_entries WHERE queue_id=$queue AND revision=$revision AND entry_id=$entry;")
[ -n "$title" ]
started=$(date +%s)
profile
act "$(labelled "OutputChip::touch" "输出到 pc1")"
for _ in $(seq 1 30); do
 state=$(sql "SELECT queue_id || '|' || revision || '|' || entry_id || '|' || playing || '|' ||
 (position_us/1000 + CASE WHEN playing THEN GREATEST(0,(extract(epoch FROM clock_timestamp())*1000)::bigint-anchor_wall_us/1000) ELSE 0 END)
 FROM play_groups WHERE account_id=$ACCOUNT_ID AND outputs=ARRAY['$TARGET'] AND '$ME'=ANY(members);")
 IFS='|' read -r got_queue got_revision got_entry playing got_position <<< "$state"
 if [ "$got_queue|$got_revision|$got_entry|$playing" = "$queue|$revision|$entry|true" ]; then break; fi
 sleep 1
done
[ "$got_queue|$got_revision|$got_entry|$playing" = "$queue|$revision|$entry|true" ]
delta=$((got_position - position - ($(date +%s)-started)*1000))
[ "$delta" -ge -7000 ] && [ "$delta" -le 7000 ]
echo "组精确接续 queue=$queue revision=$revision entry=$entry position=$got_position delta=$delta"

title_on() {
 local root win h
 CALL_PORT=$1 refresh
 h=$(CALL_PORT=$1 nth "PlayerBar::title")
 CALL_PORT=$1 call get_element_properties "{\"elementHandle\":$h}" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("accessibleLabel") or "")'
}
for _ in $(seq 1 30); do
 if [ "$(title_on "$PORT")" = "$title" ] && [ "$(title_on "$TARGET_PORT")" = "$title" ]; then break; fi
 sleep 1
done
[ "$(title_on "$PORT")" = "$title" ] && [ "$(title_on "$TARGET_PORT")" = "$title" ]
a=$(rg "自动续播轮询" "$TARGET_LOG" | tail -1)
sleep 2
b=$(rg "自动续播轮询" "$TARGET_LOG" | tail -1)
[[ "$b" == *"放空 false"* && "${a#*位置 }" != "${b#*位置 }" ]]
if [ "$PORT" = 8090 ]; then
 : "${ANDROID_SERIAL:?安卓源要设备序列号}"
 pid=$(adb -s "$ANDROID_SERIAL" shell pidof io.github.osmosis | tr -d '\r')
 [ -n "$pid" ]
 audio=$(adb -s "$ANDROID_SERIAL" shell dumpsys audio)
 if rg -q "u/pid:[0-9]+/$pid state:started" <<< "$audio"; then
  echo "源设备还在本机出声" >&2; exit 1
 fi
else
 [[ "$(rg "自动续播轮询" "$SOURCE_LOG" | tail -1)" == *"放空 true"* ]]
fi
# 遥控暂停也必须落在同一组,证明源端已是遥控器。
refresh
act "$(labelled "RoundControl::touch" "暂停")"
for _ in $(seq 1 10); do
 [ "$(sql "SELECT playing FROM play_groups WHERE account_id=$ACCOUNT_ID;")" = f ] && break
 sleep 1
done
[ "$(sql "SELECT playing FROM play_groups WHERE account_id=$ACCOUNT_ID;")" = f ]
echo "$MODE:通过;pc1 播放器位置推进,源端不再本机出声,遥控暂停已落库"
