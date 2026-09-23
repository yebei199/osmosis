#!/usr/bin/env bash
# 真机:对当前屏上的列表发鼠标滚轮,断言应用不崩、列表真的滚了(#120)。
#
# 驱动走 adb 的 `input mouse scroll`(ACTION_SCROLL,与外接鼠标同一条路),
# 断言走 PID、crash 缓冲区和应用内嵌的 MCP(真机在 8090)。
# 前提:just mcp-android 装的 debug 包在跑、已登录,目标列表已经在屏上。
#
#   ROW_ID=TrackList::touch test/wheel-scroll-android.sh      # 每日推荐 / 歌单详情
#   ROW_ID=PlaylistList::touch test/wheel-scroll-android.sh   # 我的歌单
set -euo pipefail

PORT="${PORT:-8090}"
ROW_ID="${ROW_ID:-TrackList::touch}"
PACKAGE="${PACKAGE:-io.github.osmosis}"
COUNT="${COUNT:-20}"

call() {
  curl -s -X POST "http://127.0.0.1:$PORT/mcp" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}" \
  | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]["content"][0]; print(r.get("text",""))'
}

win=$(call list_windows '{}' | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["windowHandles"][0]))')
scale=$(call get_window_properties "{\"windowHandle\":$win}" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["scaleFactor"])')

# 首行的逻辑坐标:x 取中点(滚轮打在它上面),y 用来判断滚没滚。
first_row() {
  local row
  row=$(call find_elements_by_id "{\"windowHandle\":$win,\"elementsId\":\"$ROW_ID\"}" \
    | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["elementHandles"][0]))')
  call get_element_properties "{\"elementHandle\":$row}" \
    | python3 -c 'import json,sys; p=json.load(sys.stdin); print(p["positionX"]+p["sizeWidth"]/2, p["positionY"]+p["sizeHeight"]/2)'
}

scroll() { # <逻辑 x> <逻辑 y> <格数,负数是往下滚>
  adb shell input mouse scroll \
    "$(python3 -c "print(int($1*$scale))")" "$(python3 -c "print(int($2*$scale))")" \
    --axis "VSCROLL,$3"
}

pid=$(adb shell pidof "$PACKAGE")
since=$(adb shell date +'%m-%d\ %H:%M:%S.000')
read -r x y < <(first_row)
echo "PID $pid,首行中心 ($x, $y),scale $scale"

for i in $(seq 1 "$COUNT"); do
  if [ $((i % 2)) -eq 1 ]; then scroll "$x" "$y" -1; else scroll "$x" "$y" 1; fi
done
sleep 1

now=$(adb shell pidof "$PACKAGE" || true)
crashes=$(adb logcat -b crash -d -T "$since" | grep -c "$PACKAGE" || true)
echo "滚完 $COUNT 次:PID ${now:-<无>},crash 缓冲新增 $crashes 条"
[ "$now" = "$pid" ] && [ "$crashes" -eq 0 ] || { echo "应用崩了 —— 失败" >&2; exit 1; }

# 一格正好 60px,等于行高:整格滚完首行的 y 看不出变化。拿半格探方向与幅度。
scroll "$x" "$y" -0.5
sleep 0.5
read -r _ y_down < <(first_row)
scroll "$x" "$y" 0.5
sleep 0.5
echo "半格往下后首行 y: $y -> $y_down"
python3 -c "import sys; sys.exit(0 if $y_down < $y else 1)" \
  || { echo "往下滚半格,首行没有上移 —— 列表没滚" >&2; exit 1; }

echo "通过"
