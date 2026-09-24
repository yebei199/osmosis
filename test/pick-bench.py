#!/usr/bin/env python3
"""起播计时:经应用内嵌的 MCP 点歌,记下每一下的点击时刻,再从带时间戳的日志里
把这一下的分段耗时与主线程卡顿读出来(#137 ① 的基线与删后对比)。

三个子命令:

  drive  点歌。每点一下等 play_events 多一行(有设备真起播了),再静置几秒,
         把 {index, click, played} 按行写进 --out(JSON Lines,时间是本机 epoch 秒)。
  stamp  给 stdin 的每一行前面加本机 epoch 秒。桌面把 stderr 接过它再落盘。
  report 把 drive 的输出与日志对上,每一下打一行:
         click→begin / begin→prepared / prepared→sound / sound→drawn、
         click→sound、这一下之后 --window 秒内的卡顿次数与总时长。

日志要每行带 epoch 秒:桌面用 stamp,安卓用 `adb logcat -v epoch -s osmosis`。
遥控时点的是控制端(--port),日志读的是出声的那一端;`--clock-offset` 是
日志时钟减本机时钟(安卓日志用的是手机的钟)。

    osmosis-desktop 2>&1 | test/pick-bench.py stamp > app.log
    test/pick-bench.py drive --port 8091 --mode list --picks 0,1,0 --out picks.jsonl
    test/pick-bench.py drive --port 8091 --output device --out remote.jsonl   # 遥控名字带 device 的那台
    test/pick-bench.py report --picks picks.jsonl --log app.log

前提同 pick-e2e.sh:应用起着、已登录,server-dev 与 osmosis-pg 在跑。
卡顿行要 OSMOSIS_STALL 打开才有(桌面运行期,APK 构建期)。
"""

import argparse
import json
import re
import subprocess
import sys
import time
import urllib.request

ACT = re.compile(r"act#(\d+) play (\w+) \+\d+ms total=(\d+)ms")
STALL = re.compile(r"主线程卡了 (\d+)ms")
STAMP = re.compile(r"^\s*(\d{9,}\.\d+)")


def mcp(port, name, args):
    body = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": args},
        }
    ).encode()
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/mcp",
        data=body,
        headers={
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
        },
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        text = response.read().decode()
    # 响应可能是 SSE,取 data: 那一行。
    for line in text.splitlines():
        if line.startswith("data: "):
            text = line[len("data: ") :]
            break
    content = json.loads(text)["result"]["content"][0].get("text", "")
    return json.loads(content) if content.startswith("{") else content


def handles(port, win, element_id):
    found = mcp(port, "find_elements_by_id", {"windowHandle": win, "elementsId": element_id})
    return found.get("elementHandles") or []


def must(port, win, element_id, index=0):
    found = handles(port, win, element_id)
    if len(found) <= index:
        sys.exit(f"找不到 {element_id}[{index}] —— 页面不对,或者这个构建没有它")
    return found[index]


def played_count(container):
    out = subprocess.run(
        ["docker", "exec", container, "psql", "-U", "slint", "-d", "osmosis", "-tAc",
         "select count(*) from play_events;"],
        check=True,
        capture_output=True,
        text=True,
    )
    return int(out.stdout.strip() or 0)


def open_section(port, win, section):
    # 上一轮卡墙起播后开着播放页,先收起来,不然分区与歌单都点不到。
    mcp(port, "dispatch_key_event", {"windowHandle": win, "text": "\u001b"})
    time.sleep(1)
    mcp(port, "click_element", {"elementHandle": must(port, win, "NavItem::touch", 1)})
    rail = handles(port, win, "MusicRail::item-touch") or handles(port, win, "MusicBar::item-touch")
    if len(rail) <= section:
        sys.exit(f"分区 {section} 不存在")
    mcp(port, "click_element", {"elementHandle": rail[section]})
    time.sleep(3)


def show_view(port, win, mode):
    # 卡墙起播后会开播放页,开着时卡墙不推相机;用户按 Esc 收起,这里也一样。
    mcp(port, "dispatch_key_event", {"windowHandle": win, "text": "\u001b"})
    time.sleep(1)
    toggle = handles(port, win, f"WallView::view-{mode}-btn")
    if toggle:
        mcp(port, "invoke_accessibility_action", {"elementHandle": toggle[0], "action": "Default_"})
    want = "TrackList::touch" if mode == "list" else "WallView::wall-area"
    for _ in range(60):
        if handles(port, win, want):
            return
        time.sleep(1)
    sys.exit(f"切到 {mode} 视图 60 秒没落地")


def tap(port, win, mode, index):
    if mode == "list":
        mcp(port, "click_element", {"elementHandle": must(port, win, "TrackList::touch", index)})
        return
    # 卡墙的卡画在 3D 纹理里,按坐标点不稳,走无障碍动作(见 pick-e2e.sh)。
    area = must(port, win, "WallView::wall-area")
    for _ in range(index + 1):
        mcp(port, "invoke_accessibility_action", {"elementHandle": area, "action": "Increment"})
    mcp(port, "invoke_accessibility_action", {"elementHandle": area, "action": "Default_"})


def button(port, win, predicate):
    """按无障碍标签找第一颗按钮。MCP 的查询没有按标签查的谓词,只好逐个问。"""
    root = mcp(port, "get_window_properties", {"windowHandle": win})["rootElementHandle"]
    found = mcp(
        port,
        "query_element_descendants",
        {
            "elementHandle": root,
            "findAll": True,
            "queryStack": [{"matchDescendants": True}, {"matchElementAccessibleRole": "Button"}],
        },
    )
    for handle in found.get("elementHandles") or []:
        label = mcp(port, "get_element_properties", {"elementHandle": handle}).get("accessibleLabel") or ""
        if predicate(label):
            return handle, label
    return None, None


def select_output(port, win, name):
    """在抽屉里选输出设备:`本机`,或名字里带 name 的那台(遥控它)。"""
    drawer, _ = button(port, win, lambda label: label == "更多")
    if drawer is None:
        sys.exit("找不到抽屉键")
    mcp(port, "click_element", {"elementHandle": drawer})
    time.sleep(1)
    chip, label = button(port, win, lambda label: label.startswith("输出到 ") and name in label)
    if chip is None:
        sys.exit(f"抽屉里没有名字带 {name} 的输出设备")
    mcp(port, "click_element", {"elementHandle": chip})
    print(f"输出设备: {label}")
    time.sleep(3)
    close, _ = button(port, win, lambda label: label == "收起更多")
    if close is not None:
        mcp(port, "click_element", {"elementHandle": close})


def drive(args):
    win = mcp(args.port, "list_windows", {})["windowHandles"][0]
    if args.output:
        select_output(args.port, win, args.output)
    open_section(args.port, win, args.section)
    if args.playlist is not None:
        # 大队列那一组:进「我的歌单」里的某一张,点播时整张冻进队列。
        rows = []
        for _ in range(20):
            rows = handles(args.port, win, "PlaylistList::touch")
            if len(rows) > args.playlist:
                break
            time.sleep(0.5)
        if len(rows) <= args.playlist:
            sys.exit(f"歌单 {args.playlist} 不存在")
        mcp(args.port, "click_element", {"elementHandle": rows[args.playlist]})
        time.sleep(8)
    with open(args.out, "a") as out:
        for index in (int(i) for i in args.picks.split(",")):
            show_view(args.port, win, args.mode)
            before = played_count(args.pg)
            click = time.time()
            tap(args.port, win, args.mode, index)
            played = None
            while time.time() < click + args.timeout:
                if played_count(args.pg) > before:
                    played = time.time()
                    break
                time.sleep(0.2)
            row = {"mode": args.mode, "index": index, "click": click, "played": played}
            out.write(json.dumps(row) + "\n")
            out.flush()
            print(json.dumps(row))
            time.sleep(args.settle)


def stamp(_args):
    for line in sys.stdin:
        sys.stdout.write(f"{time.time():.3f} {line}")
        sys.stdout.flush()


def read_log(path, offset):
    lines = []
    with open(path, errors="replace") as log:
        for line in log:
            found = STAMP.match(line)
            if found:
                lines.append((float(found.group(1)) - offset, line))
    return lines


def report(args):
    log = read_log(args.log, args.clock_offset)
    picks = [json.loads(line) for line in open(args.picks) if line.strip()]
    print("mode index click→begin begin→prepared prepared→sound sound→drawn click→sound stalls(n/ms)")
    for pick in picks:
        click = pick["click"]
        window = [(t, line) for t, line in log if click - 0.5 <= t <= click + args.window]
        # 这一下之后第一个 begin 的那个 act#,之后只认它的几段。
        stages, act = {}, None
        for t, line in window:
            found = ACT.search(line)
            if not found:
                continue
            if act is None and found.group(2) == "begin":
                act = found.group(1)
            if found.group(1) == act:
                stages[found.group(2)] = (t, int(found.group(3)))
        stalls = [int(m.group(1)) for _, line in window if (m := STALL.search(line))]

        def lap(a, b):
            if a in stages and b in stages:
                return str(stages[b][1] - stages[a][1])
            return "-"

        def since_click(stage):
            return f"{(stages[stage][0] - click) * 1000:.0f}" if stage in stages else "-"

        print(
            pick["mode"],
            pick["index"],
            since_click("begin"),
            lap("begin", "prepared"),
            lap("prepared", "sound"),
            lap("sound", "drawn"),
            since_click("sound"),
            f"{len(stalls)}/{sum(stalls)}",
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    d = sub.add_parser("drive")
    d.add_argument("--port", type=int, default=8091)
    d.add_argument("--mode", choices=["list", "wall"], default="list")
    d.add_argument("--section", type=int, default=0, help="音乐页分区下标,0 是每日推荐")
    d.add_argument("--output", help="先在抽屉里选输出设备:本机,或设备名的一部分(遥控那台)")
    d.add_argument("--playlist", type=int, help="配 --section 1:先进第几张歌单")
    d.add_argument("--picks", default="0,1,0", help="依次点第几首;重复的下标是缓存命中那一下")
    d.add_argument("--settle", type=float, default=8, help="起播后静置几秒再点下一首")
    d.add_argument("--timeout", type=float, default=60)
    d.add_argument("--pg", default="osmosis-pg")
    d.add_argument("--out", required=True)
    d.set_defaults(run=drive)

    s = sub.add_parser("stamp")
    s.set_defaults(run=stamp)

    r = sub.add_parser("report")
    r.add_argument("--picks", required=True)
    r.add_argument("--log", required=True)
    r.add_argument("--window", type=float, default=8, help="点击后几秒内的日志算这一下的")
    r.add_argument("--clock-offset", type=float, default=0.0)
    r.set_defaults(run=report)

    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
