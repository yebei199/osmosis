#!/usr/bin/env python3
"""迁移端到端(#137 ③):经遥控器内嵌的 MCP 选一台输出设备,盯着控制条直到迁移确认,
断言控制条全程在、曲名全程不变、确认之后进度从源停下的位置接着走。

    test/move-e2e.py --port 8091 --to 小米 --out move.jsonl [--ns-pid PID] [--watch 20]

`--to` 是抽屉里输出芯片的名字片段,`本机` 表示选回本机。`--ns-pid` 给了就经
`nsenter` 进那个网络命名空间去打 MCP(`test/ns-desktop.sh` 起的实例 MCP 只听在 ns 里)。

判据(不看截图):
  - 控制条(`PlayerBar::title`)在每一次采样里都在,曲名与选设备前一致 —— 从前选完设备
    镜像一清,控制条下一拍就销毁,而且会跳到被控端手上原来那一首;
  - 状态行出现过「正在切到」,最后消失(迁移确认了);
  - 确认之后 `--settle` 秒,控制条上的时间读数不早于选设备那一刻的读数 —— 是接着放,
    不是从 0:00 起。

每一次采样写一行 JSON 到 `--out`(本机 epoch 秒),退出码 0 = 全部判据成立。
实际出声的证据(源停了、目标响了)另取:桌面看 PipeWire 流的 corked,安卓看
`dumpsys audio` 里的 AudioTrack 状态 —— 见 `.dispatch` 下这一轮的报告。
"""

import argparse
import json
import re
import subprocess
import sys
import time


def mcp(args, name, arguments):
    body = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }
    )
    command = [
        "curl", "-s", "-X", "POST", f"http://127.0.0.1:{args.port}/mcp",
        "-H", "Content-Type: application/json",
        "-H", "Accept: application/json, text/event-stream",
        "-d", body,
    ]
    if args.ns_pid:
        command = ["nsenter", "-t", str(args.ns_pid), "-U", "-n", "--preserve-credentials"] + command
    text = subprocess.run(command, check=True, capture_output=True, text=True, timeout=30).stdout
    for line in text.splitlines():
        if line.startswith("data: "):
            text = line[len("data: "):]
            break
    content = json.loads(text)["result"]["content"][0].get("text", "")
    return json.loads(content) if content.startswith("{") else content


def handles(args, win, element_id):
    found = mcp(args, "find_elements_by_id", {"windowHandle": win, "elementsId": element_id})
    return found.get("elementHandles") or []


def text_of(args, handle):
    props = mcp(args, "get_element_properties", {"elementHandle": handle})
    return props.get("accessibleLabel") or props.get("text") or props.get("accessibleValue") or ""


def element_text(args, win, element_id):
    found = handles(args, win, element_id)
    return text_of(args, found[0]) if found else None


def button(args, win, predicate):
    root = mcp(args, "get_window_properties", {"windowHandle": win})["rootElementHandle"]
    found = mcp(
        args,
        "query_element_descendants",
        {
            "elementHandle": root,
            "findAll": True,
            "queryStack": [{"matchDescendants": True}, {"matchElementAccessibleRole": "Button"}],
        },
    )
    for handle in found.get("elementHandles") or []:
        label = text_of(args, handle)
        if predicate(label):
            return handle, label
    return None, None


def clock_seconds(text):
    """「1:03 / 3:40」里的前一个时间,秒。读不出来就 None。"""
    match = re.search(r"(\d+):(\d{2})", text or "")
    return int(match.group(1)) * 60 + int(match.group(2)) if match else None


def select(args, win):
    drawer, _ = button(args, win, lambda label: label == "更多")
    if drawer is None:
        sys.exit("找不到抽屉键 —— 控制条不在(本机没在放?)")
    mcp(args, "click_element", {"elementHandle": drawer})
    time.sleep(1)
    if args.to == "本机":
        want = lambda label: label == "输出到 本机"  # noqa: E731
    else:
        want = lambda label: label.startswith("输出到 ") and args.to in label  # noqa: E731
    chip, label = button(args, win, want)
    if chip is None:
        sys.exit(f"抽屉里没有 {args.to} 那颗输出芯片")
    clicked = time.time()
    mcp(args, "click_element", {"elementHandle": chip})
    close, _ = button(args, win, lambda label: label == "收起更多")
    if close is not None:
        mcp(args, "click_element", {"elementHandle": close})
    return clicked, label


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--port", type=int, default=8091)
    parser.add_argument("--ns-pid", type=int)
    parser.add_argument("--to", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--watch", type=float, default=20.0, help="最多盯多少秒等迁移确认")
    parser.add_argument("--settle", type=float, default=4.0, help="确认之后再等几秒读进度")
    args = parser.parse_args()

    win = mcp(args, "list_windows", {})["windowHandles"][0]
    before_title = element_text(args, win, "PlayerBar::title")
    before_clock = clock_seconds(element_text(args, win, "PlayerBar::clock"))
    if not before_title:
        sys.exit("选设备之前控制条就不在 —— 先在遥控器上放一首")

    clicked, label = select(args, win)
    print(f"选了 {label},选之前在放「{before_title}」,读数 {before_clock}s")

    failures = []
    saw_moving = False
    confirmed_at = None
    with open(args.out, "a") as out:
        while time.time() < clicked + args.watch:
            title = element_text(args, win, "PlayerBar::title")
            status = element_text(args, win, "MainWindow::move-label") or ""
            row = {"t": time.time(), "bar": title is not None, "title": title, "move": status}
            out.write(json.dumps(row, ensure_ascii=False) + "\n")
            if title is None:
                failures.append(f"{row['t']:.2f} 控制条不在")
            elif title != before_title:
                failures.append(f"{row['t']:.2f} 曲名变成了「{title}」")
            if status:
                saw_moving = True
            elif saw_moving:
                confirmed_at = time.time()
                break
            time.sleep(0.1)

    if not saw_moving:
        failures.append("状态行从没出现过「正在切到」—— 迁移没开始,或者一拍就过去了没采到")
    if confirmed_at is None:
        failures.append(f"{args.watch}s 内迁移没确认")
    else:
        time.sleep(args.settle)
        after_clock = clock_seconds(element_text(args, win, "PlayerBar::clock"))
        print(f"确认用了 {confirmed_at - clicked:.2f}s,之后 {args.settle}s 读数 {after_clock}s")
        if before_clock is not None and after_clock is not None and after_clock < before_clock:
            failures.append(f"进度从 {before_clock}s 退到了 {after_clock}s —— 不是接着放")

    for failure in failures:
        print("✗", failure)
    print("全部判据成立" if not failures else f"{len(failures)} 条不成立")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
