#!/usr/bin/env bash
# #174:真实电台起播后点 pc1 输出。只在单账号的隔离开发库造损坏引用。
# direct:切输出请求自己恢复;recovered:重入册先恢复空组,再开电台并切输出。
# 必填 ACCOUNT_ID/ME/TARGET/TARGET_PORT/TARGET_LOG/PG_CONTAINER;recovered 还要 RESTART。
# 桌面源另设 SOURCE_LOG;安卓源设 ANDROID_SERIAL。运行前应用已登录。
set -euo pipefail
MODE="${1:?用法: $0 direct|recovered|idle}"
PORT="${PORT:-8091}"
: "${ACCOUNT_ID:?要隔离测试账号 ID}" "${ME:?要源设备 ID}" "${TARGET:?要 pc1 设备 ID}"
: "${TARGET_PORT:?要 pc1 MCP 端口}" "${TARGET_LOG:?要 pc1 播放器日志}"
: "${PG_CONTAINER:?要本轮独立 Postgres 容器}"
: "${SOURCE_LOG:?要源实例日志}" "${SERVER_LOG:?要服务端日志}"
[[ "$PG_CONTAINER" == osmosis-174-* && "$ACCOUNT_ID" =~ ^[0-9]+$ ]] || exit 2
[[ "$ME" =~ ^[A-Za-z0-9_-]+$ && "$TARGET" =~ ^[A-Za-z0-9_-]+$ && "$ME" != "$TARGET" ]] || exit 2
case "$MODE" in
 direct) ;;
 recovered) : "${RESTART:?要重启源实例的命令}" ;;
 idle) : "${RESTART:?要重启源实例的命令}" "${RESTART_TARGET:?要重启目标实例的命令}" ;;
 *) exit 2 ;;
esac

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
  if [ "$label" = "$2" ] || [[ "$2" = "输出到 pc1" && "$label" == "输出到 pc1 #"* ]]; then
   echo "$h"; return
  fi
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
report() {
 sql "SELECT q.id || '|' || r.applied_revision || '|' || r.entry_id || '|' || r.position_ms
 FROM play_queues q JOIN play_queue_reports r ON r.queue_id=q.id
 WHERE q.account_id=$ACCOUNT_ID AND q.device_id='$ME' AND r.play_state='playing'
 ORDER BY r.reported_at DESC LIMIT 1;"
}
position_ms() {
 printf '%s' "$1" | python3 -c '
import re,sys
m=re.search(r"位置 ([0-9.]+)(ns|µs|ms|s), 放空 false",sys.stdin.read())
assert m, "不能解析播放器位置"
print(round(float(m[1])*{"ns":0.000001,"µs":0.001,"ms":1,"s":1000}[m[2]]))
'
}
title_on() {
 local root win h
 CALL_PORT=$1 refresh
 h=$(CALL_PORT=$1 nth "PlayerBar::title")
 CALL_PORT=$1 call get_element_properties "{\"elementHandle\":$h}" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("accessibleLabel") or "")'
}
fresh_progress() {
 local mark line previous=-1 current checkpoint_title
 local before after reference previous_reference='' previous_line='' previous_time='' sampled_at
 mark=$(wc -l < "$SOURCE_LOG")
 for _ in $(seq 1 60); do
  before=$(report)
  IFS='|' read -r queue revision entry _ <<< "$before"
  reference="$queue|$revision|$entry"
  line=$(tail -n "+$((mark+1))" "$SOURCE_LOG" | rg "自动续播轮询" | tail -1 || true)
  sampled_at=$(date -Iseconds)
  if [[ "$line" == *"放空 false"* && "$line" != "$previous_line" && "$queue" =~ ^[0-9]+$ && "$revision" =~ ^[0-9]+$ && "$entry" =~ ^[0-9]+$ ]]; then
   current=$(position_ms "$line")
   checkpoint_title=$(sql "SELECT title FROM play_queue_entries WHERE queue_id=$queue AND revision=$revision AND entry_id=$entry;")
   source_title=$(title_on "$PORT")
   after=$(report)
   if [ "$before" = "$after" ] && [ -n "$checkpoint_title" ] && [ "$source_title" = "$checkpoint_title" ]; then
    if [ "$reference" = "$previous_reference" ] && [ "$current" -ge 20000 ] && [ "$previous" -ge 0 ] && [ "$current" -gt "$previous" ]; then
     position=$current
     echo "$previous_time 源前样本: $previous_line;引用=$previous_reference;实际进度=$previous"
     echo "$sampled_at 源后样本: $line;引用=$reference;实际进度=$position"
     echo "取样前后检查点=$before;曲名=$checkpoint_title"
     return
    fi
    previous=$current
    previous_reference=$reference
    previous_line=$line
    previous_time=$sampled_at
   else
    previous=-1
    previous_reference=''
   fi
  fi
  sleep 1
 done
 echo "缺少新鲜且超过容差的本机进度" >&2; return 1
}

profile
act "$(labelled "OutputChip::touch" "输出到 本机")"
if [ "$MODE" = idle ]; then
 snapshot=$(sql "SELECT q.id || '|' || e.revision || '|' || e.entry_id FROM play_queues q
 JOIN play_queue_entries e ON e.queue_id=q.id WHERE q.account_id=$ACCOUNT_ID ORDER BY q.id DESC,e.revision DESC LIMIT 1;")
 IFS='|' read -r queue revision entry <<< "$snapshot"
 [[ "$queue" =~ ^[0-9]+$ && "$revision" =~ ^[0-9]+$ && "$entry" =~ ^[0-9]+$ ]]
else
 radio
 fresh_progress
fi
# SQL 仅造损坏现场,实际输出选择始终由 MCP 点击。
sql "INSERT INTO play_groups(account_id,version,members,outputs,queue_id,revision,entry_id,playing)
 VALUES($ACCOUNT_ID,1,ARRAY['$TARGET'],ARRAY['$TARGET'],$queue,$revision+1000000,$entry,false)
 ON CONFLICT(account_id) DO UPDATE SET version=play_groups.version+1,members=EXCLUDED.members,
 outputs=EXCLUDED.outputs,queue_id=EXCLUDED.queue_id,revision=EXCLUDED.revision,entry_id=EXCLUDED.entry_id,
 playing=false,position_us=0,anchor_wall_us=0,boundary_wall_us=NULL,alive_wall_us=NULL,play_order='{}';" >/dev/null
if [ "$MODE" != direct ]; then
 source_mark=$(wc -l < "$SOURCE_LOG")
 idle_source_mark=$source_mark
 idle_target_mark=$(wc -l < "$TARGET_LOG")
 if [ "$MODE" = idle ]; then bash -c "$RESTART_TARGET"; fi
 bash -c "$RESTART"
 for _ in $(seq 1 60); do
  if [ "$(sql "SELECT queue_id IS NULL AND revision IS NULL AND entry_id IS NULL FROM play_groups WHERE account_id=$ACCOUNT_ID;")" = t ]; then break; fi
  sleep 1
 done
 [ "$(sql "SELECT queue_id IS NULL AND revision IS NULL AND entry_id IS NULL FROM play_groups WHERE account_id=$ACCOUNT_ID;")" = t ]
 recovered_version=$(sql "SELECT version FROM play_groups WHERE account_id=$ACCOUNT_ID;")
 for _ in $(seq 1 60); do
  state_line=$(tail -n "+$((source_mark+1))" "$SOURCE_LOG" | rg "组状态: 第 $recovered_version 版," | tail -1 || true)
  [ -n "$state_line" ] && break
  sleep 1
 done
 [ -n "$state_line" ] || { echo "恢复状态未到源客户端" >&2; exit 1; }
 echo "$(date -Iseconds) 客户端恢复状态: $state_line"
 refresh
 if [ "$MODE" = recovered ]; then radio; fresh_progress; fi
fi

# 点击前再抓当前曲目与进度,不用开始电台时的旧快照。
profile
output_handle=$(labelled "OutputChip::touch" "输出到 pc1")
if [ "$MODE" != idle ]; then
 fresh_progress
 title=$(sql "SELECT title FROM play_queue_entries WHERE queue_id=$queue AND revision=$revision AND entry_id=$entry;")
 [ -n "$title" ]
fi
if [ "$MODE" = direct ]; then
 broken=$(sql "SELECT g.revision IS NOT NULL AND NOT EXISTS(SELECT 1 FROM play_queue_entries e
 WHERE e.queue_id=g.queue_id AND e.revision=g.revision AND e.entry_id=g.entry_id) FROM play_groups g WHERE g.account_id=$ACCOUNT_ID;")
 [ "$broken" = t ] || { echo "direct 点击前已经提前恢复,此格未验" >&2; exit 1; }
 echo "$(date -Iseconds) direct 点击前仍损坏: $(sql "SELECT queue_id||'|'||revision||'|'||entry_id||'|'||version FROM play_groups WHERE account_id=$ACCOUNT_ID;")"
else
 [ "$(sql "SELECT queue_id IS NULL AND revision IS NULL AND entry_id IS NULL FROM play_groups WHERE account_id=$ACCOUNT_ID;")" = t ]
fi
if [ "$MODE" = idle ]; then
 for log_mark in "$SOURCE_LOG:$idle_source_mark" "$TARGET_LOG:$idle_target_mark"; do
  latest=$(tail -n "+$((${log_mark##*:}+1))" "${log_mark%:*}" | rg "自动续播轮询" | tail -1)
  [[ "$latest" == *"放空 true"* ]] || { echo "idle 点击前并非空闲: $latest" >&2; exit 1; }
  echo "$(date -Iseconds) idle 点击前空闲: $latest"
 done
fi
target_mark=$(wc -l < "$TARGET_LOG")
source_mark=$(wc -l < "$SOURCE_LOG")
server_mark=$(wc -l < "$SERVER_LOG")
started=$(date +%s)
echo "$(date -Iseconds) 点击 pc1 输出"
act "$output_handle"
if [ "$MODE" = idle ]; then
 evidence=$(mktemp -d "${EVIDENCE_DIR:-${TMPDIR:-/tmp}}/174-idle.XXXXXX")
 sleep 10
 [ "$(sql "SELECT queue_id IS NULL AND revision IS NULL AND entry_id IS NULL AND NOT playing
 FROM play_groups WHERE account_id=$ACCOUNT_ID AND outputs=ARRAY['$TARGET'] AND '$ME'=ANY(members);")" = t ]
 index=0
 for log_mark in "$SOURCE_LOG:$source_mark" "$TARGET_LOG:$target_mark"; do
  index=$((index+1))
  window="$evidence/audio-$index.log"
  tail -n "+$((${log_mark##*:}+1))" "${log_mark%:*}" | rg "自动续播轮询" > "$window"
  [ -s "$window" ] || { echo "idle 观测窗口没有新样本" >&2; exit 1; }
  if rg -q "放空 false" "$window"; then
   echo "idle 观测窗口发生自动起播: $window" >&2; exit 1
  fi
  echo "$(date -Iseconds) idle 整窗音频真相: $window ($(wc -l < "$window") 个新样本)"
  cat "$window"
 done
 if [ "$PORT" = 8090 ]; then
  audio=$(adb -s "${ANDROID_SERIAL:?要安卓序列号}" shell dumpsys audio)
  pid=$(adb -s "$ANDROID_SERIAL" shell pidof io.github.osmosis | tr -d '\r')
  [ -n "$pid" ]
  if rg -q "u/pid:[0-9]+/$pid state:started" <<< "$audio"; then exit 1; fi
 fi
 echo "idle:通过;无本机播放时真实输出点击未自动播未知曲目"
 exit 0
fi
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
if [ "$MODE" = direct ]; then
 recovery=$(tail -n "+$((server_mark+1))" "$SERVER_LOG" | rg "清除组的失效播放引用" | tail -1)
 request=$(tail -n "+$((server_mark+1))" "$SERVER_LOG" | sed 's/\x1b\[[0-9;]*m//g' | rg '/group/outputs' | rg 'status=200' | tail -1)
 [ -n "$recovery" ] && [ -n "$request" ]
 echo "本次输出操作恢复: $recovery"
 echo "本次输出响应: $request"
fi

for _ in $(seq 1 30); do
 if [ "$(title_on "$PORT")" = "$title" ] && [ "$(title_on "$TARGET_PORT")" = "$title" ]; then break; fi
 sleep 1
done
[ "$(title_on "$PORT")" = "$title" ] && [ "$(title_on "$TARGET_PORT")" = "$title" ]
target_sample() {
 local line value expected difference
 for _ in $(seq 1 30); do
  line=$(tail -n "+$((target_mark+1))" "$TARGET_LOG" | rg "自动续播轮询" | tail -1 || true)
  if [[ "$line" == *"放空 false"* ]]; then
   value=$(position_ms "$line")
   expected=$(sql "SELECT position_us/1000 + GREATEST(0,(extract(epoch FROM clock_timestamp())*1000)::bigint-anchor_wall_us/1000)
   FROM play_groups WHERE account_id=$ACCOUNT_ID AND queue_id=$queue AND revision=$revision AND entry_id=$entry AND playing;")
   [[ "$expected" =~ ^[0-9]+$ ]]
   difference=$((value-expected))
   if [ "$difference" -ge -7000 ] && [ "$difference" -le 7000 ]; then
    echo "$(date -Iseconds) pc1 实际播放器: $line;组时间线=$expected 差值=$difference" >&2
    echo "$value"; return
   fi
  fi
  sleep 1
 done
 echo "pc1 实际播放器未对上组时间线: $line" >&2; return 1
}
a=$(target_sample)
sleep 2
b=$(target_sample)
[ "$b" -gt "$a" ]
if [ "$PORT" = 8090 ]; then
 : "${ANDROID_SERIAL:?安卓源要设备序列号}"
 pid=$(adb -s "$ANDROID_SERIAL" shell pidof io.github.osmosis | tr -d '\r')
 [ -n "$pid" ]
 audio=$(adb -s "$ANDROID_SERIAL" shell dumpsys audio)
 if rg -q "u/pid:[0-9]+/$pid state:started" <<< "$audio"; then
  echo "源设备还在本机出声" >&2; exit 1
 fi
else
 source_line=$(tail -n "+$((source_mark+1))" "$SOURCE_LOG" | rg "自动续播轮询" | tail -1)
 [[ "$source_line" == *"放空 true"* ]]
 echo "$(date -Iseconds) 源端已放空: $source_line"
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
