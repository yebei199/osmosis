#!/usr/bin/env bash
# 端到端:歌单列表与歌单详情铺满可用高度,底部只让出控制条那一段(#116)。
#
# 驱动走应用内嵌的 MCP(安卓 8090 / 桌面 8091),断言量的是元素几何,不看画面。
# 前提:应用起着、已登录(test/mcp-login.sh)、账号里至少有一个歌单。
# 控制条在不在都能跑:在的时候按 page-reserve 断言,不在的时候按页边距断言。
set -euo pipefail

PORT="${PORT:-8090}"

exec python3 - "$PORT" <<'PY'
import json, sys, time, urllib.request

URL = f"http://127.0.0.1:{sys.argv[1]}/mcp"
# 页边距 16 + 一格布局间距 12;控制条的 page-reserve = 62 + 22 * 2。
BARE, RESERVE, SPACING = 16.0, 106.0, 12.0

def call(name, **args):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": name, "arguments": args}}).encode()
    req = urllib.request.Request(URL, body, {
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream"})
    # 本机代理对 127.0.0.1 无意义,绕开它。
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    text = opener.open(req, timeout=30).read().decode()
    text = next((l[6:] for l in text.splitlines() if l.startswith("data: ")), text)
    out = json.loads(text)["result"]["content"][0].get("text", "")
    return json.loads(out) if out.startswith(("{", "[")) else out

win = call("list_windows")["windowHandles"][0]
root = call("get_window_properties", windowHandle=win)["rootElementHandle"]

def query(scope, **pred):
    r = call("query_element_descendants", elementHandle=scope, findAll=True, queryStack=[pred])
    return r.get("elementHandles", []) if isinstance(r, dict) else []

def by_id(id):
    r = call("find_elements_by_id", windowHandle=win, elementsId=id)
    return r.get("elementHandles", []) if isinstance(r, dict) else []

def props(h):
    return call("get_element_properties", elementHandle=h)

def bottom(h):
    p = props(h)
    return p["absolutePosition"].get("y", 0.0) + p["size"]["height"]

def click(h):
    call("click_element", elementHandle=h, action="SingleClick", button="Left")
    time.sleep(3)

def expect(id, bare_max):
    page = query(root, matchElementTypeName="MusicPage")[0]
    blank = bottom(page) - bottom(by_id(id)[0])
    bar = bool(query(root, matchElementTypeName="PlayerBar"))
    lo, hi = (RESERVE, RESERVE + SPACING) if bar else (0.0, bare_max)
    # 半个像素的余量:真机 scale 2.75,逻辑坐标换算回来带小数尾巴。
    ok = lo - 0.5 <= blank <= hi + 0.5
    print(f"{id}: 底下空 {blank:.2f}px(控制条{'在' if bar else '不在'},允许 {lo:g}~{hi:g})"
          f" —— {'通过' if ok else '失败'}")
    return ok

# 音乐页 → 「我的歌单」分区。分段条/竖栏里的格子没有 id,按顺序取;
# 竖栏第一格是收起键,所以往后挪一格。
for h in query(root, matchElementAccessibleRole="Button"):
    if props(h).get("accessibleLabel") == "音乐":
        click(h)
nav = by_id("MusicPage::music-bar") or by_id("MusicPage::music-rail")
cells = query(nav[0], matchElementTypeNameOrBase="TouchArea")
click(cells[1] if by_id("MusicPage::music-bar") else cells[2])
if by_id("MusicPage::playlist-header"):
    click(by_id("MusicPage::back")[0])

ok = expect("MusicPage::playlist-list", BARE + SPACING)

# 点开第一个歌单。详情的曲目要等网络,轮询到列表出现为止。
click(query(by_id("MusicPage::playlist-list")[0], matchElementTypeNameOrBase="TouchArea")[0])
for _ in range(20):
    if by_id("MusicPage::track-list"):
        break
    time.sleep(0.5)
else:
    sys.exit("详情里一直没有曲目列表 —— 歌单是空的,还是没取到?")
ok = expect("MusicPage::track-list", BARE + SPACING) and ok
click(by_id("MusicPage::back")[0])

sys.exit(0 if ok else 1)
PY
