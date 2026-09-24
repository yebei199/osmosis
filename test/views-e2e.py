#!/usr/bin/env python3
"""浏览视图各存各的(#137 ④):经应用内嵌的 MCP 进「我的歌单 → 我喜欢的」,边进边采样
列表里摆的是哪几首,断言别的视图的歌一次都没出现。

    test/views-e2e.py --port 8091 --out views.jsonl [--steps enter,aba,relogin]

不点任何一首歌 —— 全程不出声。三步:

  enter    先进每日推荐记下它那一批(D),再进我的歌单 → 我喜欢的;
  aba      我喜欢的 → 另一个歌单 → 我喜欢的,中间不等加载;
  relogin  设置页退出登录,`test/mcp-login.sh` 登回来,再进我喜欢的。

判据(不看截图):每一步最后一次点击之后持续采样 `--watch` 秒,每次采样读列表前几行的
无障碍标签(曲目行的 accessible-label 就是歌名)。采样要么是空列表(加载态,
`MusicPage::empty-loading` 在),要么每一首都属于这一步收尾时我喜欢的那一批(L);
出现 L 之外的歌就判失败 —— 尤其是 D 里的。收尾时的 L 必须非空。

每次采样写一行 JSON 到 `--out`,退出码 0 = 全部判据成立。
"""

import argparse
import json
import os
import subprocess
import sys
import time

LIKED = "我喜欢的"
ROWS = 4  # 每次采样读前几行:一次采样要 1 + ROWS 次调用,多读就慢


def mcp(args, name, arguments):
    body = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }
    )
    text = subprocess.run(
        [
            "curl", "-s", "-X", "POST", f"http://127.0.0.1:{args.port}/mcp",
            "-H", "Content-Type: application/json",
            "-H", "Accept: application/json, text/event-stream",
            "-d", body,
        ],
        check=True, capture_output=True, text=True, timeout=30,
    ).stdout
    for line in text.splitlines():
        if line.startswith("data: "):
            text = line[len("data: "):]
            break
    content = json.loads(text)["result"]["content"][0].get("text", "")
    return json.loads(content) if content.startswith("{") else content


def window(args):
    return mcp(args, "list_windows", {})["windowHandles"][0]


def handles(args, element_id):
    found = mcp(args, "find_elements_by_id", {"windowHandle": args.win, "elementsId": element_id})
    return found.get("elementHandles") or [] if isinstance(found, dict) else []


def label(args, handle):
    props = mcp(args, "get_element_properties", {"elementHandle": handle})
    # 句柄在两次调用之间失效了(行被重建),回的是一句报错
    return props.get("accessibleLabel") or "" if isinstance(props, dict) else None


def click(args, handle):
    mcp(args, "click_element", {"elementHandle": handle})


def button(args, wanted):
    root = mcp(args, "get_window_properties", {"windowHandle": args.win})["rootElementHandle"]
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
        if label(args, handle) == wanted:
            return handle
    return None


def wait_for(what, probe, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = probe()
        if value:
            return value
        time.sleep(0.3)
    sys.exit(f"等不到{what}")


def sample(args):
    """列表前几行的歌名,以及加载态在不在。"""
    rows = handles(args, "TrackList::touch")[:ROWS]
    titles = [label(args, row) for row in rows]
    loading = bool(handles(args, "MusicPage::empty-loading"))
    return {"t": round(time.time(), 3), "titles": [t for t in titles if t], "loading": loading}


def section(args, index):
    items = wait_for("分区导航", lambda: handles(args, "MusicRail::item-touch") or handles(args, "MusicBar::item-touch"))
    click(args, items[index])


def open_playlist(args, name=None, index=None):
    """点开一个歌单,直到歌单索引真的让位给详情页。

    登录刚回来时歌单列表会被 /playlists 的回包整个重建一次,点在旧行上的那一下
    就丢了 —— 所以点完要确认换了页,没换就重点(最多三次,次数照实打出来)。
    """
    for attempt in range(3):
        def find():
            rows = handles(args, "PlaylistList::touch")
            if index is not None:
                return rows[index] if len(rows) > index else None
            return next((row for row in rows if label(args, row) == name), None)

        click(args, wait_for(f"歌单「{name if index is None else index}」", find))
        deadline = time.time() + 3
        while time.time() < deadline:
            if not handles(args, "PlaylistList::touch"):
                return
            time.sleep(0.05)
        print(f"点开歌单没生效,重点第 {attempt + 1} 次")
    sys.exit("歌单点了三次都没打开")


def to_music(args):
    if handles(args, "MusicRail::item-touch") or handles(args, "MusicBar::item-touch"):
        return
    # 主导航第二格就是音乐(Nav.items 的下标 1);找不到再退到 Home 页上的音乐磁贴
    tabs = handles(args, "NavItem::touch")
    music = tabs[1] if len(tabs) > 1 else wait_for("音乐入口", lambda: button(args, "音乐"))
    click(args, music)


def watch(args, step, out):
    """最后一次点击之后持续采样,收尾时的那批当 L,逐条判。"""
    samples = []
    deadline = time.time() + args.watch
    while time.time() < deadline:
        samples.append(sample(args))
    final = samples[-1]["titles"] if samples else []
    liked = set(final)
    bad = [s for s in samples if any(t not in liked for t in s["titles"])]
    for s in samples:
        s["step"] = step
        s["foreign"] = [t for t in s["titles"] if t not in liked]
        out.write(json.dumps(s, ensure_ascii=False) + "\n")
    loading = sum(1 for s in samples if s["loading"])
    print(f"{step}: 采样 {len(samples)} 次,加载态 {loading} 次,L 前几首 {final},越界 {len(bad)} 次")
    return bool(final) and not bad, liked, bad


def step_enter(args, out):
    section(args, 0)
    # GPU 构建的推荐分区默认是卡墙,曲目行不在元素树里;切到列表才读得到歌名
    as_list = wait_for("推荐分区", lambda: handles(args, "WallView::view-list-btn") or sample(args)["titles"])
    if isinstance(as_list, list) and as_list and isinstance(as_list[0], dict):
        click(args, as_list[0])
    daily = set(wait_for("每日推荐的曲目", lambda: sample(args)["titles"]))
    print(f"enter: 每日推荐前几首 {sorted(daily)}")
    section(args, 1)
    open_playlist(args, name=LIKED)
    ok, liked, bad = watch(args, "enter", out)
    leaked = [t for s in bad for t in s["foreign"] if t in daily]
    if leaked:
        print(f"enter: 进我喜欢的时出现过每日推荐的歌 {sorted(set(leaked))}")
    return ok


def step_aba(args, out):
    section(args, 1)
    open_playlist(args, name=LIKED)
    section(args, 1)
    open_playlist(args, index=1)
    section(args, 1)
    open_playlist(args, name=LIKED)
    ok, _, _ = watch(args, "aba", out)
    return ok


def step_relogin(args, out):
    # 宽版式的设置是侧栏底下那颗带标签的圆钮;紧凑版式是底栏第四格(Nav.items 两项
    # 之后接着排 bottom-items,设置是其中第二项)
    tabs = handles(args, "NavItem::touch")
    to_settings = button(args, "设置") or (tabs[3] if len(tabs) > 3 else None)
    if to_settings is None:
        sys.exit("等不到设置入口")
    click(args, to_settings)
    logout = wait_for("退出登录键", lambda: button(args, "退出登录"))
    click(args, logout)
    wait_for("登录页", lambda: handles(args, "LoginPage::username"))
    env = dict(os.environ, PORT=str(args.port))
    subprocess.run([os.path.join(os.path.dirname(__file__), "mcp-login.sh")], check=True, env=env)
    to_music(args)
    section(args, 1)
    open_playlist(args, name=LIKED)
    ok, _, _ = watch(args, "relogin", out)
    return ok


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8091)
    parser.add_argument("--out", required=True)
    parser.add_argument("--watch", type=float, default=12)
    parser.add_argument("--steps", default="enter,aba,relogin")
    args = parser.parse_args()
    args.win = window(args)

    steps = {"enter": step_enter, "aba": step_aba, "relogin": step_relogin}
    results = {}
    with open(args.out, "a", encoding="utf-8") as out:
        to_music(args)
        for name in args.steps.split(","):
            results[name] = steps[name](args, out)
    print(json.dumps(results, ensure_ascii=False))
    sys.exit(0 if all(results.values()) else 1)


if __name__ == "__main__":
    main()
