"""从真实界面点歌，数据库确认三连点只起播、发布一次；保留旧入口模式。"""

import argparse
import os
import shlex
import subprocess
import time
from pathlib import Path

from mcp_client import Client, expect


def sql(env, statement):
    """读取调用者指定开发库，任何执行失败直接传播。"""
    return subprocess.run(
        [
            "docker",
            "exec",
            env.get("PG_CONTAINER", "osmosis-pg"),
            "psql",
            "-U",
            "slint",
            "-d",
            env.get("PG_DB", "osmosis"),
            "-tAc",
            statement,
        ],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def snapshot(env):
    """保留起播、发布、检查点与组版本的原始判据。"""
    return (
        int(sql(env, "select count(*) from play_events")),
        int(sql(env, "select coalesce(sum(revision),0) from play_queues")),
        sql(env, "select coalesce(max(reported_at)::text,'') from play_queue_reports"),
        int(sql(env, "select coalesce(sum(version),0) from play_groups")),
    )


def escape(client):
    """通过用户返回键收起播放页。"""
    client.call("dispatch_key_event", windowHandle=client.window(), text="\u001b")


def reset(client, mode, keyword):
    """从音乐页开始，分区只按标签选择；保留搜索路径。"""
    for _ in range(3):
        escape(client)
    client.destination("搜索" if mode == "search" else "每日推荐")
    if mode == "search":
        box = client.by_id("MusicPage::keyword").handle()
        client.call("click_element", elementHandle=box)
        client.call("set_element_value", elementHandle=box, value=keyword)
        client.call("dispatch_key_event", windowHandle=client.window(), text="\n")


def hide_keyboard(client, env):
    """安卓搜索输入后仅在键盘真的打开时收起；不可达直接失败。"""
    port = int(env.get("PORT", "8091"))
    if port != int(env.get("ANDROID_MCP_PORT", "8090")):
        return
    adb = shlex.split(env.get("ADB", "adb"))
    subprocess.run([*adb, "shell", "true"], check=True, capture_output=True)
    result = subprocess.run(
        [*adb, "shell", "dumpsys", "input_method"], check=True, capture_output=True, text=True
    )
    if "mInputShown=true" in result.stdout:
        subprocess.run([*adb, "shell", "input", "keyevent", "KEYCODE_BACK"], check=True)


def show_view(client, mode):
    """等待列表/卡墙真正出现，不能用按钮高亮代替。"""
    escape(client)
    toggle = "WallView::view-wall-btn" if mode == "wall" else "WallView::view-list-btn"
    button = client.by_id(toggle)
    if button.all():
        button.action("Default_")
    elif mode == "wall":
        raise AssertionError("wall requires GPU build and wall toggle")
    client.by_id("WallView::wall-area" if mode == "wall" else "TrackList::touch").handle()


def queue_row(client, index):
    """队列页常驻，按类型限定行查询的作用域。"""
    page = client.elements([{"matchElementTypeName": "QueuePage"}])[0]
    return client.call(
        "query_element_descendants",
        elementHandle=page,
        findAll=True,
        queryStack=[{"matchElementId": "TrackList::touch"}],
    )["elementHandles"][index]


def open_queue(client):
    """兼容队列已打开；只在播放页不存在时点击封面。"""
    if not client.elements([{"matchElementTypeName": "PlayPage"}]):
        client.by_id("PlayerBar::cover-touch").click()
    pages = client.elements([{"matchElementTypeName": "QueuePage"}])
    if (
        pages
        and client.call("get_element_properties", elementHandle=pages[0])
        .get("absolutePosition", {})
        .get("x", 1)
        < 1
    ):
        return
    client.by_id("PlayPage::queue-entry-touch").click()
    expect(lambda: bool(client.elements([{"matchElementTypeName": "QueuePage"}])))


def tap(client, mode, index):
    """三次发送同一目标的动作；卡墙走无障碍接口。"""
    if mode == "wall":
        wall = client.by_id("WallView::wall-area")
        for _ in range(index + 1):
            wall.action("Increment")
        handle = wall.handle()
        for _ in range(3):
            client.call("invoke_accessibility_action", elementHandle=handle, action="Default_")
        return
    handle = (
        queue_row(client, index)
        if mode == "queue"
        else client.by_id("TrackList::touch", index).handle()
    )
    for _ in range(3):
        client.call("click_element", elementHandle=handle)


def both_follow(client, other, env):
    """组状态以数据库为准，两个真实客户端都必须显示那首曲目。"""
    title = sql(
        env,
        """select e.title from play_groups g join play_queue_entries e
        on (e.queue_id,e.revision,e.entry_id)=(g.queue_id,g.revision,g.entry_id)
        order by g.version desc limit 1""",
    )
    assert title, "group has no current title"
    for app in (client, other):
        expect(
            lambda app=app: (
                app.by_id("PlayerBar::title").properties().get("accessibleLabel")
                or app.by_id("PlayerBar::title").properties().get("accessibleValue")
            ),
            equals=title,
        )


def run_pick(client, mode, env):
    """保持原脚本真相源，播放计数不变时才尝试第二首。"""
    if mode not in {"list", "wall", "search", "queue"}:
        raise ValueError(f"unknown mode: {mode}")
    reset(client, mode, env.get("KEYWORD", "晴天"))
    if mode == "search":
        hide_keyboard(client, env)
    for index in range(2):
        if mode == "queue":
            open_queue(client)
        elif mode != "search":
            show_view(client, mode)
        before = snapshot(env)
        tap(client, mode, index)
        deadline = time.monotonic() + 60
        while snapshot(env)[0] == before[0] and time.monotonic() < deadline:
            time.sleep(1)
        if snapshot(env)[0] == before[0]:
            continue
        other_port = env.get("OTHER_PORT")
        if not other_port:
            deadline = time.monotonic() + 10
            while snapshot(env)[1] <= before[1] and time.monotonic() < deadline:
                time.sleep(1)
        # 观察迟到的重复发布，原脚本的三秒稳定期不缩短。
        time.sleep(3)
        after = snapshot(env)
        assert after[0] - before[0] == 1, f"play_events: {before} -> {after}"
        if other_port:
            assert after[3] - before[3] == 1, f"group version: {before} -> {after}"
            both_follow(client, Client(f"http://127.0.0.1:{other_port}/mcp"), env)
        else:
            publications = after[1] - before[1]
            assert publications == 1 or (publications == 0 and after[2] != before[2]), (
                f"publication/checkpoint: {before} -> {after}"
            )
        return
    raise AssertionError(f"{mode}: neither candidate started playback")


def main():
    """兼容旧 pick-e2e.sh 的参数和环境。"""
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=["list", "wall", "search", "queue"])
    args = parser.parse_args()
    client = Client(f"http://127.0.0.1:{os.environ.get('PORT', '8091')}/mcp", timeout=60)
    try:
        run_pick(client, args.mode, os.environ)
    except (AssertionError, RuntimeError, subprocess.SubprocessError):
        client.capture(Path(os.environ.get("MCP_ARTIFACTS", "results/mcp-failures")))
        raise
    print(f"{args.mode}: passed")


if __name__ == "__main__":
    main()
