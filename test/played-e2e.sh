#!/usr/bin/env bash
# 端到端:起播一首歌,断言 play_events 真的多了一行。
#
# 驱动走应用内嵌的 MCP(桌面在 8091),断言走数据库,不靠人看画面。
# 前提:just desktop-dev 已经起来、已登录,server 与 osmosis-pg 在跑。
set -euo pipefail

PORT="${PORT:-8091}"
PG_CONTAINER="${PG_CONTAINER:-osmosis-pg}"

call() {
  curl -s -X POST "http://127.0.0.1:$PORT/mcp" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}" \
  | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]["content"][0]; print(r.get("text",""))'
}

rows() {
  docker exec "$PG_CONTAINER" psql -U slint -d osmosis -tAc \
    "select count(*) from play_events;" | tr -d '[:space:]'
}

win=$(call list_windows '{}' | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["windowHandles"][0]))')
echo "窗口: $win"

# 音乐页 → 列表视图 → 点第一行起播。元素按 id 找,不量坐标。
music=$(call query_element_descendants \
  "{\"elementHandle\":$win,\"findAll\":true,\"queryStack\":[{\"matchDescendants\":true},{\"matchElementId\":\"NavItem::touch\"}]}" \
  | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["elementHandles"][1]))')
call click_element "{\"elementHandle\":$music}" >/dev/null
sleep 2

list=$(call find_elements_by_id "{\"windowHandle\":$win,\"elementsId\":\"MainWindow::view-list-btn\"}" \
  | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["elementHandles"][0]))')
call click_element "{\"elementHandle\":$list}" >/dev/null
sleep 2

before=$(rows)
echo "起播前 play_events: $before"

# 两个候选行轮着试。上一轮跑完之后,那一行正放着,再点它不会重新起播
# (界面按「正在放的就是这首」把它当多余的点击),账本自然不动 —— 只固定点
# 第一行的话,这个脚本第二次跑必然失败,而失败的是脚本不是功能。
for index in 0 1; do
  row=$(call find_elements_by_id "{\"windowHandle\":$win,\"elementsId\":\"TrackList::touch\"}" \
    | python3 -c "import json,sys; print(json.dumps(json.load(sys.stdin)['elementHandles'][$index]))")
  call click_element "{\"elementHandle\":$row}" >/dev/null
  echo "点了第 $((index + 1)) 行"

  # 取直链 + 开流 + 解码,再加轮询那一秒。
  for _ in $(seq 1 15); do
    sleep 1
    now=$(rows)
    if [ "$now" -gt "$before" ]; then
      echo "起播后 play_events: $now —— 通过"
      docker exec "$PG_CONTAINER" psql -U slint -d osmosis -tAc \
        "select platform, track_id, played_at from play_events order by played_at desc limit 1;"
      exit 0
    fi
  done
done

echo "起播后 play_events 仍是 $before —— 失败" >&2
exit 1
