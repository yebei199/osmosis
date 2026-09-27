#!/usr/bin/env bash
# 端到端:三态赞踩与收听时长上报(#157)。驱动走应用内嵌的 MCP,断言走数据库。
#
#   test/feedback-e2e.sh verdict   点赞 → track_feedback +1(verdict=1);
#                                   再点踩 → 同一行变 -1;再点一次踩 → 行消失。
#   test/feedback-e2e.sh skip      起播后立刻切歌,断言这一行的 listened_ms 记进去了、
#                                   且小于 30000(#157 的跳过口径:前 30 秒内切走)。
#
# 播满 = 完播那一步不进这个脚本:等一整首歌放完在自动化里太慢,那一步走
# AGENTS.md「发版与实机」的真机人工过一遍,库里查 listened_ms/duration_ms 的比值。
#
# 前提同 test/pick-e2e.sh:应用起着并已登录、just server-dev 与 osmosis-pg 在跑、
# 每日推荐有歌。
set -euo pipefail

MODE="${1:?用法: $0 verdict|skip}"
PORT="${PORT:-8091}"
PG_CONTAINER="${PG_CONTAINER:-osmosis-pg}"

call() {
  curl -s -X POST "http://127.0.0.1:${PORT}/mcp" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}" \
  | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]["content"][0]; print(r.get("text",""))'
}

handle() {
  call query_element_descendants "{\"elementHandle\":$root,\"findAll\":true,\"queryStack\":[{\"matchElementId\":\"$1\"}]}" \
  | python3 -c "
import json, sys
hs = json.load(sys.stdin).get('elementHandles') or []
print(json.dumps(hs[${2:-0}]) if len(hs) > ${2:-0} else '')
"
}

must() {
  [ -n "$1" ] || { echo "找不到 $2 —— 页面不对,或者手上没有正在放的歌" >&2; exit 1; }
}

present() {
  call query_element_descendants "{\"elementHandle\":$root,\"findAll\":false,\"queryStack\":[{\"matchElementTypeName\":\"$1\"}]}" \
    | python3 -c 'import json,sys; print("yes" if json.load(sys.stdin).get("elementHandles") else "")'
}

sql() {
  docker exec "$PG_CONTAINER" psql -U slint -d osmosis -tAc "$1" | tr -d '[:space:]'
}
feedback_count() { sql "select count(*) from track_feedback;"; }
last_feedback_verdict() {
  sql "select verdict from track_feedback order by updated_at desc limit 1;"
}
played() { sql "select count(*) from play_events;"; }
last_listened_ms() {
  sql "select coalesce(listened_ms::text, 'NULL') from play_events order by id desc limit 1;"
}

# 复位:播放页收起 → 音乐页 → 每日推荐,点第一行起播。**不**展开播放页 ——
# skip 模式接下来要点列表里的另一行切歌,播放页开着会盖住列表。
start_playing() {
  local music item row
  for _ in 1 2 3; do
    [ -n "$(present PlayPage)" ] || break
    call dispatch_key_event "{\"windowHandle\":$win,\"text\":\"\\u001b\"}" >/dev/null
    sleep 1
  done
  music=$(handle "NavItem::touch" 1)
  must "$music" "音乐入口"
  call click_element "{\"elementHandle\":$music}" >/dev/null
  item=$(handle "MusicRail::item-touch" 0)
  [ -n "$item" ] || item=$(handle "MusicBar::item-touch" 0)
  must "$item" "音乐页第一个分区"
  call click_element "{\"elementHandle\":$item}" >/dev/null
  sleep 2

  row=$(handle "TrackList::touch" 0)
  must "$row" "列表第一行"
  call click_element "{\"elementHandle\":$row}" >/dev/null
}

# 展开播放页:点控制条封面。只有 verdict 模式要它 —— 赞踩键长在播放页上。
open_play_page() {
  local cover
  for _ in $(seq 1 30); do
    [ -n "$(handle "PlayerBar::cover-touch")" ] && break
    sleep 1
  done
  cover=$(handle "PlayerBar::cover-touch")
  must "$cover" "控制条封面"
  call click_element "{\"elementHandle\":$cover}" >/dev/null
  sleep 1
}

win=$(call list_windows '{}' | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["windowHandles"][0]))')
root=$(call get_window_properties "{\"windowHandle\":$win}" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["rootElementHandle"]))')

start_playing

case "$MODE" in
  verdict)
    open_play_page
    for _ in $(seq 1 20); do
      [ -n "$(handle "PlayPage::feedback-up")" ] && break
      sleep 1
    done
    up=$(handle "PlayPage::feedback-up")
    down=$(handle "PlayPage::feedback-down")
    must "$up" "点赞键"
    must "$down" "点踩键"

    before=$(feedback_count)
    call click_element "{\"elementHandle\":$up}" >/dev/null
    sleep 1
    after=$(feedback_count)
    [ "$((after - before))" -eq 1 ] || { echo "verdict: 失败 —— 点赞该多一行(前 $before,后 $after)" >&2; exit 1; }
    [ "$(last_feedback_verdict)" = "1" ] || { echo "verdict: 失败 —— 新行该是 +1" >&2; exit 1; }
    echo "  点赞: track_feedback +1,verdict=1"

    call click_element "{\"elementHandle\":$down}" >/dev/null
    sleep 1
    [ "$(feedback_count)" -eq "$after" ] || { echo "verdict: 失败 —— 改点踩不该多一行" >&2; exit 1; }
    [ "$(last_feedback_verdict)" = "-1" ] || { echo "verdict: 失败 —— 该覆盖成 -1" >&2; exit 1; }
    echo "  改点踩: 同一行变 -1"

    call click_element "{\"elementHandle\":$down}" >/dev/null
    sleep 1
    [ "$(feedback_count)" -eq "$before" ] || { echo "verdict: 失败 —— 再点一次该取消(行消失)" >&2; exit 1; }
    echo "  再点踩: 行消失"
    echo "verdict: 通过"
    ;;
  skip)
    for _ in $(seq 1 30); do
      [ "$(played)" -gt 0 ] && break
      sleep 1
    done
    [ "$(played)" -gt 0 ] || { echo "skip: 失败 —— 没起播" >&2; exit 1; }

    # 上一首/下一首键没有独立的元素 id 可按(与 PlayerBar::cover-touch 不同名),
    # 等自动续播又太慢 —— 直接切到列表第二行来触发一次切歌。
    row=$(handle "TrackList::touch" 1)
    must "$row" "列表第二行(用来触发一次切歌)"
    call click_element "{\"elementHandle\":$row}" >/dev/null
    sleep 2

    ms=$(last_listened_ms)
    [ "$ms" != "NULL" ] || { echo "skip: 失败 —— 切歌后上一行的 listened_ms 该被补上,读到 NULL" >&2; exit 1; }
    [ "$ms" -lt 30000 ] || { echo "skip: 失败 —— 起播后立刻切歌,listened_ms 该小于 30000,读到 $ms" >&2; exit 1; }
    echo "  切歌后 listened_ms=$ms(< 30000,判定为跳过)"
    echo "skip: 通过"
    ;;
  *)
    echo "用法: $0 verdict|skip" >&2
    exit 2
    ;;
esac
