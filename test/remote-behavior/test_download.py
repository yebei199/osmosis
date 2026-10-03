"""桌面真实选歌与下载入口到公共目录成品的独占环境验收。"""

import json
import os
import subprocess
import tempfile
import threading
import time
from pathlib import Path

import pytest

from media import DAILY_IDS, Media
from resources import Client, World, wait_until


# 只替换外部音源：提供可逐字节比较的 MP3，生产 server 与客户端照常传输。
@pytest.fixture
def download_world(monkeypatch):
    directory = Path(tempfile.mkdtemp(prefix="169-", dir=os.environ["REMOTE_BEHAVIOR_ARTIFACTS"]))
    track = directory / "source.mp3"
    subprocess.run(
        [
            "ffmpeg",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=3",
            "-c:a",
            "libmp3lame",
            str(track),
        ],
        check=True,
        capture_output=True,
    )
    content = track.read_bytes()
    release = threading.Event()
    transfer = threading.Event()
    original_dispatch = Media.dispatch
    original_handler = Media.handler
    original_start = Client.start

    # HOME 由公共客户端夹具隔离，这里只指定该客户端的公共 Music 落点。
    def music_start(client):
        (client.directory / "config/user-dirs.dirs").write_text('XDG_MUSIC_DIR="$HOME/Music"\n')
        return original_start(client)

    # 正常播放仍用原夹具，点击下载之前才切换这一次外部音源的行为。
    def dispatch(media, method, request, context):
        mode = getattr(media, "download_mode", None)
        if (
            method != "GetPlaySource"
            or mode is None
            or request.level != media.pb.QUALITY_LEVEL_HIGH
            or request.track_id not in DAILY_IDS[:2]
        ):
            return original_dispatch(media, method, request, context)
        with media.lock:
            media.calls.append({"method": method, "request": str(request), "download_mode": mode})
        return media.pb.GetPlaySourceResponse(
            source=media.pb.PlaySource(
                url=f"http://127.0.0.1:{media.http.server_port}/169.mp3",
                format="mp3",
                size=len(content),
                duration_ms=3000,
                trial=mode == "trial",
            )
        )

    # 断流在真实下载写入部分字节后关闭本轮音源 HTTP 连接。
    def handler(media):
        base = original_handler(media)

        class Handler(base):
            def do_GET(self):
                if self.path != "/169.mp3":
                    return super().do_GET()
                self.send_response(200)
                self.send_header("Content-Type", "audio/mpeg")
                self.send_header("Content-Length", str(len(content)))
                self.end_headers()
                try:
                    if media.download_mode == "cut":
                        self.wfile.write(content[:1024])
                        self.wfile.flush()
                        transfer.set()
                        if not release.wait(30):
                            raise TimeoutError("download fault was not released")
                    else:
                        self.wfile.write(content)
                except (BrokenPipeError, ConnectionResetError):
                    self.log_message("download client disconnected")

        return Handler

    monkeypatch.setattr(Client, "start", music_start)
    monkeypatch.setattr(Media, "dispatch", dispatch)
    monkeypatch.setattr(Media, "handler", handler)
    root = Path(__file__).resolve().parents[2]
    target = Path(os.environ["REMOTE_BEHAVIOR_TARGET_DIR"])
    world = World(root, directory, target / "debug/osmosis-desktop", target / "debug/server")
    started = time.time()
    try:
        world.start()
        yield world, content, transfer, release
    finally:
        release.set()
        world.close()
        (directory / "download-testcase.json").write_text(
            json.dumps(
                {
                    "start": started,
                    "end": time.time(),
                    "candidate": os.environ["REMOTE_BEHAVIOR_COMMIT"],
                }
            )
        )


# 每个版式都从真实选歌开始，通过同一个用户可见抽屉下载当前曲目。
def start_download(world, mode, outcome, index=0):
    client = world.controller
    client.ui.pick(mode, index=index)
    # 等真实播放输出，避免下载夹具抢走尚未完成的播放音源请求。
    world.sound(client, DAILY_IDS[index], group=False)
    wait_until(lambda: bool(world.sql("SELECT id FROM play_events")), "selected track play event")
    world.media.download_mode = outcome

    def open_drawer():
        try:
            client.ui.activate(client.ui.must("RoundControl::touch", "更多"))
            return True
        except RuntimeError as error:
            if "element that was destroyed" not in str(error):
                raise
            return False

    wait_until(open_drawer, "current playback drawer")
    client.ui.activate(client.ui.must("DrawerRow::touch", "下载这一首"))
    return client.directory / "home/Music/osmosis"


# 提示取自真实横幅 Text 的无障碍标签，不把回调意图当下载结果。
def banner_text(client):
    handles = client.ui.elements("MainWindow::banner")
    if not handles:
        return ""
    tree = client.ui.call("get_element_tree", elementHandle=handles[0], maxElements=1000)
    assert not tree["truncated"]
    (client.directory / "download-banner.json").write_text(json.dumps(tree, ensure_ascii=False))
    return "\n".join(element.get("accessibleLabel", "") for element in tree["elements"])


# 两条 UI 入口各自贯通到完整可解码 MP3，文件内容也须与固定音源一致。
@pytest.mark.parametrize("mode", ("list", "wall"))
def test_download_creates_complete_mp3(download_world, mode):
    world, content, _, _ = download_world
    directory = start_download(world, mode, "complete")
    wait_until(lambda: directory.exists() and bool(list(directory.glob("*.mp3"))), "published mp3")
    files = list(directory.iterdir())
    assert len(files) == 1, "download left extra pending files"
    assert files[0].read_bytes() == content
    probe = subprocess.run(
        [
            "ffprobe",
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
            str(files[0]),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    assert 2.9 < float(json.loads(probe.stdout)["format"]["duration"]) < 3.2
    (world.directory / "ffprobe.json").write_text(probe.stdout)


# 截断本轮音源连接，不修改宿主网络；断流后无成品也无半截文件。
def test_interrupted_download_discards_partial_file(download_world):
    world, _, transfer, release = download_world
    directory = start_download(world, "list", "cut")
    assert transfer.wait(15), "source never sent partial bytes"
    wait_until(
        lambda: directory.exists() and any(p.stat().st_size > 0 for p in directory.iterdir()),
        "partial bytes written",
    )
    release.set()
    wait_until(
        lambda: not world.controller.ui.elements("MainWindow::download-strip"),
        "download error completed",
    )
    assert list(directory.iterdir()) == [], "interrupted download left files"


# 外部 trial 标记走生产 server 拒绝，UI 保留原提示，待定条目被丢弃。
def test_trial_download_keeps_existing_refusal(download_world):
    world, _, _, _ = download_world
    directory = start_download(world, "list", "trial")
    wait_until(lambda: "只有试听片段" in banner_text(world.controller), "trial-only refusal")
    assert directory.exists()
    assert list(directory.iterdir()) == [], "trial response became a saved song"


# 已下载入口沿音乐 tab 与同一分段条走，避免直接构造管理状态。
def open_downloads(client):
    client.ui.call("dispatch_key_event", windowHandle=client.ui.window, text="\x1b")
    navigation = client.ui.elements("NavItem::touch")
    client.ui.activate(navigation[1], pointer=True)
    handle = client.ui.find("MusicRail::item-touch", "已下载")
    if handle is None:
        handle = client.ui.must("MusicBar::item-touch", "已下载")
    client.ui.activate(handle)
    client.ui.must("DownloadManager::keyword")


# 文案由真实元素树取回；截断和缺失都视为检查失败。
def manager_tree(client):
    handle = client.ui.must("MusicPage::downloads-manager")
    tree = client.ui.call("get_element_tree", elementHandle=handle, maxElements=1000)
    assert not tree["truncated"]
    (client.directory / "manager-tree.json").write_text(json.dumps(tree, ensure_ascii=False))
    return tree


# 行本身的无障碍标签是发布文件名，不访问管理器内部模型。
def download_names(client):
    return {
        client.ui.call("get_element_properties", elementHandle=handle)["accessibleLabel"]
        for handle in client.ui.elements("DownloadManager::entry-touch")
    }


# 本地搜索使用真实输入框的 change 事件。
def search_downloads(client, keyword):
    client.ui.call(
        "set_element_value", elementHandle=client.ui.must("DownloadManager::keyword"), value=keyword
    )


# 按文件名选中真实条目，筛选与次序变化不影响身份。
def select_download(client, name):
    client.ui.activate(client.ui.must("DownloadManager::entry-touch", name))


# 自家删除确认按钮只在有选择时可操作。
def ask_delete(client):
    client.ui.activate(client.ui.must("HoverButton::touch", "删除所选"))


# 取消和确认分别走可见模态中的明确动作。
def answer_delete(client, confirm):
    client.ui.activate(client.ui.must("HoverButton::touch", "确认删除" if confirm else "取消"))


# 总占用取整份目录，显示精度是两位MB小数。
def assert_download_total(client, count, size):
    tree = manager_tree(client)
    labels = {item.get("accessibleLabel", "") for item in tree["elements"]}
    assert f"共 {count} 首 · 占用 {size / (1024 * 1024):.2f} MB" in labels


# 每个用户入口先下载两首真实成品，再通过管理分区删一首。
@pytest.mark.parametrize("mode", ("list", "wall"))
def test_download_manager_search_and_delete_selected_track(download_world, mode):
    world, _, _, _ = download_world
    directory = start_download(world, mode, "complete")
    wait_until(lambda: len(list(directory.glob("*.mp3"))) == 1, "first publication")
    first = next(directory.glob("*.mp3"))
    start_download(world, "list", "complete", index=1)
    wait_until(lambda: len(list(directory.glob("*.mp3"))) == 2, "second publication")
    second = next(path for path in directory.glob("*.mp3") if path != first)
    originals = {path: path.read_bytes() for path in (first, second)}
    outside = directory.parent / "178-outside.mp3"
    outside.write_bytes(b"outside-sentinel")
    client = world.controller
    open_downloads(client)
    wait_until(lambda: download_names(client) == {first.name, second.name}, "download list")
    assert_download_total(client, 2, sum(len(data) for data in originals.values()))
    search_downloads(client, "rb-" + DAILY_IDS[0])
    wait_until(lambda: download_names(client) == {first.name}, "local search")
    select_download(client, first.name)
    ask_delete(client)
    assert first.exists() and second.exists()
    answer_delete(client, True)
    wait_until(lambda: not first.exists(), "selected file deleted")
    assert second.read_bytes() == originals[second]
    assert outside.read_bytes() == b"outside-sentinel"
    search_downloads(client, "")
    wait_until(lambda: download_names(client) == {second.name}, "remaining file listed")
    assert_download_total(client, 1, len(originals[second]))
    wait_until(
        lambda: (
            f"已删除 1 首,释放 {len(originals[first]) / (1024 * 1024):.2f} MB"
            in banner_text(client)
        ),
        "actual deletion result",
    )


# 两首同时选择，取消保留原字节；再次确认后全部真删除且外部哨兵保持。
def test_download_manager_cancel_then_delete_multiple(download_world):
    world, _, _, _ = download_world
    directory = start_download(world, "list", "complete")
    wait_until(lambda: len(list(directory.glob("*.mp3"))) == 1, "first publication")
    start_download(world, "list", "complete", index=1)
    wait_until(lambda: len(list(directory.glob("*.mp3"))) == 2, "second publication")
    originals = {path: path.read_bytes() for path in directory.glob("*.mp3")}
    outside = directory.parent / "178-outside.mp3"
    outside.write_bytes(b"outside-sentinel")
    client = world.controller
    open_downloads(client)
    wait_until(lambda: download_names(client) == {path.name for path in originals}, "download list")
    for path in originals:
        select_download(client, path.name)
    ask_delete(client)
    answer_delete(client, False)
    for path, content in originals.items():
        assert path.read_bytes() == content
    assert_download_total(client, 2, sum(map(len, originals.values())))
    ask_delete(client)
    answer_delete(client, True)
    wait_until(lambda: not any(path.exists() for path in originals), "both files deleted")
    assert outside.read_bytes() == b"outside-sentinel"
    wait_until(lambda: not download_names(client), "empty download list")
    assert_download_total(client, 0, 0)
