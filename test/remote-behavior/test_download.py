"""桌面真实选歌与下载入口到公共目录成品的独占环境验收。"""

import json
import os
import subprocess
import tempfile
import threading
import time
from pathlib import Path

import pytest

from media import Media
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

    # 每个客户端的 HOME 与 Music 都属于本 testcase。
    def isolated_start(client):
        home = client.directory / "home"
        home.mkdir(exist_ok=True)
        client.env["HOME"] = str(home)
        (client.directory / "config/user-dirs.dirs").write_text('XDG_MUSIC_DIR="$HOME/Music"\n')
        return original_start(client)

    # 正常播放仍用原夹具，点击下载之前才切换这一次外部音源的行为。
    def dispatch(media, method, request, context):
        mode = getattr(media, "download_mode", None)
        if method != "GetPlaySource" or mode is None:
            return original_dispatch(media, method, request, context)
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

    monkeypatch.setattr(Client, "start", isolated_start)
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
def start_download(world, mode, outcome):
    client = world.controller
    client.ui.pick(mode)
    client.ui.must("RoundControl::touch", "更多")
    # 选歌 HTTP 请求完成后才设置下载源，避免改变播放阶段的外部输入。
    wait_until(lambda: bool(world.sql("SELECT id FROM play_events")), "selected track play event")
    world.media.download_mode = outcome
    client.ui.activate(client.ui.must("RoundControl::touch", "更多"))
    client.ui.activate(client.ui.must("DrawerRow::touch", "下载这一首"))
    return client.directory / "home/Music/osmosis"


# 提示取自真实元素树的 Text 属性，不把回调意图当下载结果。
def banner_text(client):
    tree = client.ui.call("get_element_tree", elementHandle=client.ui.root)
    return json.dumps(tree, ensure_ascii=False)


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
