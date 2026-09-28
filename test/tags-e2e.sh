#!/usr/bin/env bash
# 端到端:标签(#158)。长按一首歌打开「标签…」,新建一个标签立即打上,
# 再点掉它,断言两次都落到 track_tags 表里而不是只在界面上看着对。
#
#   test/tags-e2e.sh
#
# 驱动走应用内嵌的 MCP(桌面 8091 / 真机 8090),断言走数据库。
#
# 长按没有专门的手势可发:MCP 只有 click_element(按下松开几毫秒内完成)与
# drag_element(按下→插值移动→松开)。tracklist.slint 的长按判据是「按住不放
# 450ms」,不看有没有移动,所以拖到一个足够远的落点、靠插值步数撑够 450ms
# 真实耗时,和真的按住不放效果一样——短距离的 drag 同样只有几毫秒,不够。
#
# 前提:应用起着(桌面 just desktop-dev,安卓 just mcp-android)、已登录、
# just server-dev 与 osmosis-pg 在跑、每日推荐有歌。
set -euo pipefail

PORT="${PORT:-8091}"
PG_CONTAINER="${PG_CONTAINER:-osmosis-pg}"
TAG_NAME="${TAG_NAME:-e2e-$$-$(date +%s)}"

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
  [ -n "$1" ] || { echo "找不到 $2 —— 页面不对,或者手上没有歌" >&2; exit 1; }
}

act() {
  call invoke_accessibility_action "{\"elementHandle\":$1,\"action\":\"Default_\"}" >/dev/null
}

sql() {
  docker exec "$PG_CONTAINER" psql -U slint -d osmosis -tAc "$1" | tr -d '[:space:]'
}
tag_rows() {
  sql "select count(*) from track_tags tt join tags t on t.id = tt.tag_id where t.name = '$TAG_NAME';"
}

win=$(call list_windows '{}' | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["windowHandles"][0]))')
root=$(call get_window_properties "{\"windowHandle\":$win}" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["rootElementHandle"]))')

# 复位到音乐页的每日推荐:播放页收起、点音乐入口。不展开播放页——长按要的是
# 列表里的一行,播放页开着会盖住它。
for _ in 1 2 3; do
  present=$(call query_element_descendants "{\"elementHandle\":$root,\"findAll\":false,\"queryStack\":[{\"matchElementTypeName\":\"PlayPage\"}]}" \
    | python3 -c 'import json,sys; print("yes" if json.load(sys.stdin).get("elementHandles") else "")')
  [ -n "$present" ] || break
  call dispatch_key_event "{\"windowHandle\":$win,\"text\":\"\\u001b\"}" >/dev/null
  sleep 1
done
music=$(handle "NavItem::touch" 1)
must "$music" "音乐入口"
call click_element "{\"elementHandle\":$music}" >/dev/null
sleep 1

row=$(handle "TrackList::touch" 0)
must "$row" "列表第一行"
props=$(call get_element_properties "{\"elementHandle\":$row}")
title=$(echo "$props" | python3 -c 'import json,sys; print(json.load(sys.stdin)["accessibleLabel"])')
x=$(echo "$props" | python3 -c 'import json,sys; p=json.load(sys.stdin); print(p["absolutePosition"]["x"] + p["size"]["width"]/2)')
y=$(echo "$props" | python3 -c 'import json,sys; p=json.load(sys.stdin); print(p["absolutePosition"]["y"] + p["size"]["height"]/2)')
# 落点摆到窗口下方老远:距离够长,插值步数撑够 450ms 的长按判据(见文件头注释)。
far_y=$(python3 -c "print($y + 5000)")

before=$(tag_rows)

call drag_element "{\"elementHandle\":$row,\"target\":{\"x\":$x,\"y\":$far_y},\"button\":\"Left\"}" >/dev/null

found=""
for h in $(call query_element_descendants "{\"elementHandle\":$root,\"findAll\":true,\"queryStack\":[{\"matchElementTypeName\":\"TouchArea\"}]}" \
  | python3 -c 'import json,sys; [print(json.dumps(h, separators=(",",":"))) for h in json.load(sys.stdin).get("elementHandles") or []]'); do
  label=$(call get_element_properties "{\"elementHandle\":$h}" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("accessibleLabel") or "")')
  if [ "$label" = "标签" ]; then found="$h"; break; fi
done
must "$found" "长按菜单里的「标签」项 —— 长按没触发,或者菜单形状变了"
act "$found"
sleep 1

input=$(call query_element_descendants "{\"elementHandle\":$root,\"findAll\":true,\"queryStack\":[{\"matchElementTypeName\":\"LineEdit\"}]}" \
  | python3 -c 'import json,sys; hs=json.load(sys.stdin).get("elementHandles") or []; print(json.dumps(hs[0]) if hs else "")')
must "$input" "新建标签输入框 —— 选择器没打开"
call set_element_value "{\"elementHandle\":$input,\"value\":\"$TAG_NAME\"}" >/dev/null
sleep 0.3

create=""
for h in $(call query_element_descendants "{\"elementHandle\":$root,\"findAll\":true,\"queryStack\":[{\"matchElementTypeName\":\"TouchArea\"}]}" \
  | python3 -c 'import json,sys; [print(json.dumps(h, separators=(",",":"))) for h in json.load(sys.stdin).get("elementHandles") or []]'); do
  label=$(call get_element_properties "{\"elementHandle\":$h}" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("accessibleLabel") or "")')
  if [ "$label" = "新建" ]; then create="$h"; break; fi
done
must "$create" "「新建」键"
act "$create"
sleep 1

[ "$(tag_rows)" -eq "$((before + 1))" ] \
  || { echo "失败 —— 新建标签该给「$title」多一行 track_tags(前 $before,后 $(tag_rows))" >&2; exit 1; }
echo "  新建「$TAG_NAME」并打到「$title」上:track_tags +1"

# 再点一次同一行取消勾选,断言级联清空(与摘标签同一条路)。
box=""
for h in $(call query_element_descendants "{\"elementHandle\":$root,\"findAll\":true,\"queryStack\":[{\"matchElementAccessibleRole\":\"Checkbox\"}]}" \
  | python3 -c 'import json,sys; [print(json.dumps(h, separators=(",",":"))) for h in json.load(sys.stdin).get("elementHandles") or []]'); do
  label=$(call get_element_properties "{\"elementHandle\":$h}" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("accessibleLabel") or "")')
  if [ "$label" = "$TAG_NAME" ]; then box="$h"; break; fi
done
must "$box" "刚新建的那一行勾选框"
act "$box"
sleep 1

[ "$(tag_rows)" -eq "$before" ] \
  || { echo "失败 —— 再点一次该取消,track_tags 该回到 $before,读到 $(tag_rows)" >&2; exit 1; }
echo "  再点一次取消勾选:track_tags 回到打标签之前"
echo "通过"
