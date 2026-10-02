"""外部音乐来源及输出 PCM 的独立判据，所有业务执行留在真实应用。"""

import concurrent.futures
import hashlib
import importlib
import json
import os
import subprocess
import sys
import threading
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from itertools import pairwise
from pathlib import Path

import grpc
import numpy as np

SOURCE_RATE = 24000
CAPTURE_RATE = 48000
DURATION = 90
FRAME_SECONDS = 0.125
SILENCE_RMS = 0.0005
MIN_SIGNAL_RMS = 0.015
FREQUENCY_TOLERANCE = 9
POSITION_TOLERANCE = 1.5
DAILY_IDS = ("175001", "175002", "175003")


# 两个声道分别标识曲目和媒体秒数，不能靠请求日志生成目标输出。
def frequencies(track_id: str, second: int) -> tuple[int, int]:
    return 320 + (int(track_id) - 175001) * 160, 2400 + second * 24


# WAV 固定输入让流式读取、解码、重采样及 seek 都经过真实播放器。
def write_track(path: Path, track_id: str) -> None:
    with wave.open(str(path), "wb") as output:
        output.setparams((2, 2, SOURCE_RATE, 0, "NONE", "not compressed"))
        t = np.arange(SOURCE_RATE) / SOURCE_RATE
        for second in range(DURATION):
            left, right = frequencies(track_id, second)
            samples = np.stack(
                (np.sin(2 * np.pi * left * t), np.sin(2 * np.pi * right * t)), axis=1
            )
            output.writeframes((samples * 8000).astype("<i2").tobytes())


# 每次运行从仓库的实际 proto 生成绑定，禁止手写一份不同的线上协议。
def load_proto(root: Path, directory: Path):
    directory.mkdir()
    subprocess.run(
        [
            sys.executable,
            "-m",
            "grpc_tools.protoc",
            f"-I{root / 'server/proto'}",
            f"--python_out={directory}",
            f"--grpc_python_out={directory}",
            str(root / "server/proto/music/v1/music.proto"),
        ],
        check=True,
        timeout=30,
    )
    sys.path.insert(0, str(directory))
    return importlib.import_module("music.v1.music_pb2")


# 只替换外部提供方，使用真实 gRPC framing 与仓库生成的 protobuf。
class Media:
    def __init__(self, root: Path, directory: Path):
        self.directory = directory
        self.pb = load_proto(root, directory / "proto")
        self.lock = threading.Lock()
        self.fm_turn = 0
        self.calls = []
        self.sources = {}
        workers = int(os.environ.get("REMOTE_BEHAVIOR_RPC_WORKERS", "1"))
        if workers < 1:
            raise ValueError("fixture RPC workers must be positive")
        self.http = ThreadingHTTPServer(("127.0.0.1", 0), self.handler())
        self.http_thread = threading.Thread(target=self.http.serve_forever, daemon=True)
        self.pool = concurrent.futures.ThreadPoolExecutor(max_workers=workers)
        self.grpc = grpc.server(self.pool)
        try:
            self.grpc.add_generic_rpc_handlers(
                [
                    self.service(name)
                    for name in (
                        "AuthService",
                        "CatalogService",
                        "DiscoverService",
                        "LibraryService",
                    )
                ]
            )
            self.grpc_port = self.grpc.add_insecure_port("127.0.0.1:0")
            if not self.grpc_port:
                raise RuntimeError("fixture gRPC listener did not bind")
            self.grpc.start()
            self.http_thread.start()
        except Exception:
            # 构造失败时 World 尚未取得此对象，必须在原异常重抛前关闭自有资源。
            self.grpc.stop(0).wait(timeout=5)
            self.http.server_close()
            self.pool.shutdown(wait=True, cancel_futures=True)
            raise

    # 素材身份由输入决定，HTTP 请求只负责取字节。
    def track(self, track_id):
        return self.pb.Track(
            platform=1,
            id=track_id,
            title=f"RB-{track_id}",
            duration_ms=DURATION * 1000,
            artists=[self.pb.Artist(id="175", name="RemoteBehavior")],
            quality=self.pb.Quality(
                codec="pcm", sample_rate=SOURCE_RATE, bits_per_sample=16, channels=2
            ),
        )

    # 请求日志仅作链路辅证，最终播放断言来自独立 monitor。
    def dispatch(self, method, request, context):
        with self.lock:
            self.calls.append({"method": method, "request": str(request)})
        pb = self.pb
        if method == "GetAccountStatus":
            return pb.GetAccountStatusResponse(logged_in=True, user_id="175", nickname="RB")
        if method in ("GetDailyRecommendations", "SearchTracks", "GetTracks"):
            ids = list(request.track_ids) if method == "GetTracks" else DAILY_IDS
            fields = {"tracks": [self.track(i) for i in ids]}
            if method == "SearchTracks":
                fields["total"] = len(ids)
            return getattr(pb, f"{method}Response")(**fields)
        if method in ("GetPersonalFm", "GetIntelligenceList"):
            with self.lock:
                turn = self.fm_turn
                self.fm_turn += 1
            if turn >= 15:
                context.abort(grpc.StatusCode.RESOURCE_EXHAUSTED, "fixture FM budget exhausted")
            ids = [str(175004 + turn * 3 + i) for i in range(3)]
            return getattr(pb, f"{method}Response")(tracks=[self.track(i) for i in ids])
        if method == "GetPlaySource":
            path = self.source(request.track_id)
            return pb.GetPlaySourceResponse(
                source=pb.PlaySource(
                    url=f"http://127.0.0.1:{self.http.server_port}/{request.track_id}.wav",
                    format="wav",
                    size=path.stat().st_size,
                    bit_rate=SOURCE_RATE * 32,
                    level=4,
                    expires_in_seconds=3600,
                    duration_ms=DURATION * 1000,
                )
            )
        if method == "GetLyric":
            return pb.GetLyricResponse(lyric=pb.Lyric())
        if method in ("GetRecommendedPlaylists", "ListUserPlaylists", "ListLikedTracks"):
            return getattr(pb, f"{method}Response")()
        context.abort(grpc.StatusCode.UNIMPLEMENTED, f"unplanned fixture request: {method}")

    # 自动取方法签名，新增上游调用会明确失败而不会返回万能成功。
    def service(self, name):
        handlers = {}
        for method in self.pb.DESCRIPTOR.services_by_name[name].methods:
            if method.server_streaming:
                continue
            request_type = getattr(self.pb, method.input_type.name)
            response_type = getattr(self.pb, method.output_type.name)
            handlers[method.name] = grpc.unary_unary_rpc_method_handler(
                lambda request, context, method=method.name: self.dispatch(
                    method, request, context
                ),
                request_deserializer=request_type.FromString,
                response_serializer=response_type.SerializeToString,
            )
        return grpc.method_handlers_generic_handler(f"bangdream.music.v1.{name}", handlers)

    # 路径只接受夹具编号，资源上限避免电台失控耗尽磁盘。
    def source(self, track_id):
        if not track_id.isdecimal() or not 175001 <= int(track_id) < 175049:
            raise ValueError(f"unexpected media identity: {track_id}")
        with self.lock:
            path = self.directory / f"{track_id}.wav"
            if track_id not in self.sources:
                write_track(path, track_id)
                self.sources[track_id] = hashlib.sha256(path.read_bytes()).hexdigest()
        return path

    # Range 与长度元数据使后退 seek 也走生产流式读取机制。
    def handler(self):
        media = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                track_id = self.path.removeprefix("/").removesuffix(".wav")
                path = media.source(track_id)
                size = path.stat().st_size
                start, end = 0, size - 1
                requested = self.headers.get("Range")
                if requested:
                    if not requested.startswith("bytes=") or "," in requested:
                        self.send_error(416)
                        return
                    left, right = requested[6:].split("-", 1)
                    start = int(left)
                    end = min(int(right), end) if right else end
                    if start > end or start >= size:
                        self.send_error(416)
                        return
                self.send_response(206 if requested else 200)
                self.send_header("Content-Type", "audio/wav")
                self.send_header("Content-Length", str(end - start + 1))
                self.send_header("Accept-Ranges", "bytes")
                if requested:
                    self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
                self.end_headers()
                try:
                    with path.open("rb") as stream:
                        stream.seek(start)
                        remaining = end - start + 1
                        while remaining:
                            block = stream.read(min(65536, remaining))
                            self.wfile.write(block)
                            remaining -= len(block)
                except (BrokenPipeError, ConnectionResetError):
                    # seek 和切歌会主动取消旧流，断开仍记在 HTTP 日志。
                    self.log_message("cancelled stream %s", self.path)

            def log_message(self, format: str, *args) -> None:
                with media.lock:
                    media.calls.append({"http": format % args})

        return Handler

    # 服务与线程都有自己的句柄，停资源不靠进程名匹配。
    def close(self):
        self.grpc.stop(0).wait(timeout=5)
        self.pool.shutdown(wait=True, cancel_futures=True)
        self.http.shutdown()
        self.http.server_close()
        self.http_thread.join(timeout=5)
        (self.directory / "media-inputs.json").write_text(
            json.dumps(
                {
                    "hashes": self.sources,
                    "calls": self.calls,
                    "rate": SOURCE_RATE,
                    "duration": DURATION,
                },
                indent=2,
            )
        )


# 每段 PCM 独立识别两个声道；纯音幅度、频率和时间编码都有断言。
def identify(pcm: bytes, track_id: str) -> list[int]:
    samples = np.frombuffer(pcm, dtype="<i2").reshape(-1, 2) / 32768
    width = int(CAPTURE_RATE * FRAME_SECONDS)
    if len(samples) < width * 4:
        raise AssertionError("audio: fewer than four complete PCM frames")
    seconds = []
    for offset in range(0, len(samples) - width + 1, width):
        frame = samples[offset : offset + width]
        rms = np.sqrt(np.mean(frame * frame, axis=0))
        if np.min(rms) < MIN_SIGNAL_RMS:
            raise AssertionError(f"audio: signal too quiet: {rms.tolist()}")
        spectrum = np.abs(np.fft.rfft(frame * np.hanning(width)[:, None], axis=0))
        peaks = np.argmax(spectrum, axis=0) * CAPTURE_RATE / width
        wanted, _ = frequencies(track_id, 0)
        if abs(peaks[0] - wanted) > FREQUENCY_TOLERANCE:
            raise AssertionError(f"audio: wrong track: want {track_id}/{wanted}, got {peaks}")
        second = round((peaks[1] - 2400) / 24)
        if not 0 <= second < DURATION or abs(peaks[1] - (2400 + second * 24)) > FREQUENCY_TOLERANCE:
            raise AssertionError(f"audio: invalid position marker: {peaks}")
        seconds.append(second)
    if any(b < a or b - a > 1 for a, b in pairwise(seconds)):
        raise AssertionError(f"audio: position discontinuity: {seconds}")
    return seconds


# 静音也要求采到真实连续 PCM，零字节、没启动录音都判失败。
def quiet(pcm: bytes) -> None:
    if len(pcm) < CAPTURE_RATE * 4:
        raise AssertionError("audio: silence assertion lacks one second of PCM")
    samples = np.frombuffer(pcm, dtype="<i2") / 32768
    rms = float(np.sqrt(np.mean(samples * samples)))
    peak = float(np.max(np.abs(samples)))
    if rms > SILENCE_RMS or peak > SILENCE_RMS * 5:
        raise AssertionError(f"audio: non-target output is audible: rms={rms}, peak={peak}")
