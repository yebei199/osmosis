#!/usr/bin/env bash
# 真机:对当前屏上的列表发鼠标滚轮,断言应用不崩、列表真的滚了(#120)。
#
# 滚轮来自系统自带的 `hid` 工具注册的一只虚拟 USB 鼠标:报告走内核 → InputReader →
# ACTION_SCROLL,与外接鼠标同一条路。不用 `input mouse scroll` —— Android 14 的
# input 没有这个命令,而且它报 Unknown command 时退出码仍是 0。
# 断言走 PID、logcat 和应用内嵌的 MCP(真机在 8090)。只看 PID 不够:android-activity
# 接住输入回调里的 panic,Rust 那条 UI 线程死了而进程还在 —— 界面冻住,PID 一个字不变。
# 前提:just mcp-android 装的 debug 包在跑、已登录,目标列表已经在屏上并停在顶端。
#
#   test/wheel-scroll-android.sh                                 # 每日推荐 / 歌单详情
#   LIST_ID=MusicPage::playlist-list test/wheel-scroll-android.sh # 我的歌单
set -euo pipefail

PORT="${PORT:-8090}"
LIST_ID="${LIST_ID:-MusicPage::track-list}"
PACKAGE="${PACKAGE:-io.github.osmosis}"
COUNT="${COUNT:-20}"
REMOTE_JSON=/data/local/tmp/osmosis-wheel.json

call() {
  curl -s --max-time 10 -X POST "http://127.0.0.1:$PORT/mcp" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}" \
  | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]["content"][0]; print(r.get("text",""))'
}

first() { python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["elementHandles"][0]))'; }

win=$(call list_windows '{}' | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["windowHandles"][0]))')
scale=$(call get_window_properties "{\"windowHandle\":$win}" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["scaleFactor"])')
list=$(call find_elements_by_id "{\"windowHandle\":$win,\"elementsId\":\"$LIST_ID\"}" | first)

# 瞄准点(物理像素):列表横向正中、顶端往下 90 逻辑 px,落在前两行里。
read -r tx ty < <(call get_element_properties "{\"elementHandle\":$list}" | python3 -c "
import json,sys; p=json.load(sys.stdin); a=p['absolutePosition']; s=p['size']
print(int((a['x']+s['width']/2)*$scale), int((a['y']+min(90, s['height']/2))*$scale))")

# 列表里第一个 Text 是首个实例化行的标题。ListView 只实例化看得见的行,
# 滚过一整行它就换人 —— 一格滚轮 60px 正好一行高,量行的 y 看不出位移。
top_title() {
  local text
  text=$(call query_element_descendants \
    "{\"elementHandle\":$list,\"findAll\":true,\"queryStack\":[{\"matchDescendants\":true},{\"matchElementTypeName\":\"Text\"}]}" | first)
  call get_element_properties "{\"elementHandle\":$text}" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["accessibleLabel"])'
}

# 注册虚拟鼠标 → 光标推到左上角 → 慢速挪到瞄准点(快了有指针加速)→ 逐格滚。
# 参数是每格的方向,-1 往下(内容上移),1 往上。hid 进程退出时设备自动注销。
wheel() {
  python3 - "$tx" "$ty" "$@" > "$json" <<'EOF'
import json, sys
tx, ty, steps = int(sys.argv[1]), int(sys.argv[2]), [int(s) for s in sys.argv[3:]]
# 三键 + X/Y/滚轮各一字节(有符号)的标准鼠标报告描述符。
desc = [5,1,9,2,161,1,9,1,161,0,5,9,25,1,41,3,21,0,37,1,149,3,117,1,129,2,149,1,117,5,129,1,
        5,1,9,48,9,49,9,56,21,129,37,127,117,8,149,3,129,6,192,192]
ev = [{"command": "register", "name": "osmosis wheel test", "vid": 4660, "pid": 22136,
       "bus": "usb", "descriptor": desc}, {"command": "delay", "duration": 1500}]
report = lambda dx, dy, w: ev.extend([{"command": "report", "report": [0, dx & 255, dy & 255, w & 255]},
                                      {"command": "delay", "duration": 20}])
for _ in range(40):
    report(-127, -127, 0)
while tx > 0 or ty > 0:
    dx, dy = min(tx, 5), min(ty, 5)
    report(dx, dy, 0)
    tx, ty = tx - dx, ty - dy
for w in steps:
    report(0, 0, w)
    ev.append({"command": "delay", "duration": 80})
ev.append({"command": "delay", "duration": 500})
print("\n".join(json.dumps({"id": 1, **e}) for e in ev))
EOF
  adb push "$json" "$REMOTE_JSON" >/dev/null 2>&1
  adb shell hid "$REMOTE_JSON" >/dev/null
}

json=$(mktemp)
trap 'rm -f "$json"; adb shell rm -f "$REMOTE_JSON" || true' EXIT

pid=$(adb shell pidof "$PACKAGE")
since=$(adb shell date +'%m-%d\ %H:%M:%S.000')
title0=$(top_title)
echo "PID $pid,瞄准 ($tx, $ty) 物理像素,首行「$title0」"

steps=()
for i in $(seq 1 "$COUNT"); do steps+=($((i % 2 ? -1 : 1))); done
wheel "${steps[@]}"
sleep 1

now=$(adb shell pidof "$PACKAGE" || true)
crashes=$(adb logcat -b crash -d -T "$since" | grep -c "$PACKAGE" || true)
panics=$(adb logcat -d -T "$since" --pid="$pid" | grep -E "RustPanic|panicked at" || true)
echo "滚完 $COUNT 格:PID ${now:-<无>},crash 缓冲新增 $crashes 条"
[ -z "$panics" ] || { echo "$panics"; echo "应用 panic 了 —— 失败" >&2; exit 1; }
[ "$now" = "$pid" ] && [ "$crashes" -eq 0 ] || { echo "应用崩了 —— 失败" >&2; exit 1; }

# 停在顶端时往上滚是空转,所以「往下两格首行换人、再往上两格换回来」同时验了方向:
# 符号反了的话,往下那两格会顶在顶端一行也不动。
wheel -1 -1
title_down=$(top_title)
wheel 1 1
title_back=$(top_title)
echo "往下两格首行「$title_down」,再往上两格「$title_back」"
[ "$title_down" != "$title0" ] || { echo "往下滚了首行没换 —— 列表没滚" >&2; exit 1; }
[ "$title_back" = "$title0" ] || { echo "往上滚没回到原处" >&2; exit 1; }
echo "通过"
