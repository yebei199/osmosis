"""本轮独占资源：真实 server、客户端 namespace、数据库与虚拟音频。"""

import asyncio
import getpass
import json
import os
import signal
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
from dataclasses import asdict
from pathlib import Path
from typing import Any
from urllib.parse import quote

import psycopg
import psycopg.rows

from lifecycle import OwnedCommand
from media import CAPTURE_RATE, DURATION, POSITION_TOLERANCE, Media, identify, quiet
from position import (
    MIN_ADVANCE,
    MIN_POSITION,
    PAUSE_HOLD,
    Observation,
    check,
    continuation,
    resumed,
)

TIMEOUT = 40
POLL = 0.1
PCM_WINDOW = 1.25
# 默认 monitor 约2s批量到达；100ms采集使窗口观测与短缓冲输出及时对齐。
CAPTURE_LATENCY_MS = 100


# 环境失败与最终行为失败分开，故障灵敏度只认后者。
class AudioFailure(AssertionError):
    pass


# 有界条件等待；异常只有调用方明确认作尚未就绪时才转成 False。
def wait_until(predicate, label, timeout=TIMEOUT):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        threading.Event().wait(POLL)
    raise TimeoutError(f"not ready: {label}")


# 读取本进程持有的实际监听 socket，server 可绑定 port 0。
def listener_port(process):
    process.alive()
    proc = Path("/proc") / str(process.child.pid)
    owned = set()
    for fd in (proc / "fd").iterdir():
        try:
            owned.add(fd.readlink().name)
        except FileNotFoundError:
            # server 初始化期间关闭 FD 是正常竞争，不重建或重试实例。
            continue
    for line in (proc / "net/tcp").read_text().splitlines()[1:]:
        parts = line.split()
        if parts[3] == "0A" and f"socket:[{parts[9]}]" in owned:
            return int(parts[1].split(":")[1], 16)
    return None


# IPC guardian 持有资源树，leader 提前退出也不会丢掉后代归属。
class Process:
    def __init__(self, directory, name, command, env=None, stdout=None, pass_fds=()):
        self.name = name
        self.command = [str(arg) for arg in command]
        self.started = time.time()
        self.log = (directory / f"{name}.log").open("wb")
        try:
            self.child = OwnedCommand(
                self.command,
                directory / f"{name}-cleanup.json",
                env=env,
                stdout=stdout if stdout is not None else self.log,
                stderr=self.log,
                pass_fds=pass_fds,
            )
        except (OSError, RuntimeError, TimeoutError):
            self.log.close()
            raise
        self.stopped = None

    # 就绪等待期间发现提前退出立即报环境错误，不重试整个实例。
    def alive(self):
        if self.child.poll() is not None:
            raise RuntimeError(f"{self.name} exited before assertion: {self.child.returncode}")

    # 始终回收 guardian 的后代；不能用 leader 已退出作为清理成功判据。
    def stop(self):
        if self.stopped is not None:
            return
        try:
            self.child.stop()
            self.stopped = time.time()
        finally:
            self.log.close()

    # 凭据只记录命令与身份，测试密码和 token 不进命令参数。
    def receipt(self):
        return {
            "name": self.name,
            "command": self.command,
            "pid": self.child.pid,
            "start": self.started,
            "end": self.stopped,
            "exit": self.child.returncode,
            "guardian_pid": self.child.guardian.pid,
            "cleanup": self.child.result,
        }


# 信令闸只破坏本客户端的连接，HTTP 与媒体通路继续转发。
class Gate:
    def __init__(self, server_port):
        self.server_port = server_port
        self.blocked = False
        self.connections = set()
        self.limbo = []
        self.loop = asyncio.new_event_loop()
        self.thread = threading.Thread(target=self.loop.run_forever, daemon=True)
        self.thread.start()
        self.server = asyncio.run_coroutine_threadsafe(self.bind(), self.loop).result(timeout=8)
        self.port = self.server.sockets[0].getsockname()[1]

    # 每个客户端各绑定自己的临时监听端口。
    async def bind(self):
        return await asyncio.start_server(self.serve, "127.0.0.1", 0)

    # websocket 升级之后也透明转发字节，鉴权和业务响应仍由真实 server 产生。
    async def serve(self, reader, writer):
        pair = None
        server_writer = None
        try:
            head = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), timeout=8)
            is_signal = head.startswith(b"GET /signal")
            if is_signal and self.blocked:
                return
            server_reader, server_writer = await asyncio.open_connection(
                "127.0.0.1", self.server_port
            )
            server_writer.write(head)
            await server_writer.drain()
            pair = (writer, server_writer)
            if is_signal:
                self.connections.add(pair)
            await asyncio.gather(self.pump(reader, server_writer), self.pump(server_reader, writer))
        except (OSError, asyncio.IncompleteReadError, asyncio.LimitOverrunError, TimeoutError):
            # 断线是这里唯一施加的环境故障；连接生命周期仍精确清理。
            pass
        finally:
            writer.close()
            if pair:
                self.connections.discard(pair)
            if server_writer and server_writer not in self.limbo:
                server_writer.close()

    # 单方向 EOF 使另一方向收口，故障注入保留的半连接除外。
    async def pump(self, reader, writer):
        try:
            while data := await reader.read(65536):
                writer.write(data)
                await writer.drain()
        finally:
            if writer not in self.limbo:
                writer.close()

    # 把切断安排在闸自己的 loop，避免对异步 socket 跨线程操作。
    def cut(self):
        async def change():
            self.blocked = True
            for writer, upstream in list(self.connections):
                self.limbo.append(upstream)
                writer.transport.abort()

        asyncio.run_coroutine_threadsafe(change(), self.loop).result(timeout=8)

    # 恢复只准入新连接，由生产客户端自己的重连逻辑恢复。
    def heal(self):
        self.loop.call_soon_threadsafe(setattr, self, "blocked", False)

    # 停掉 listener 与连接再退出线程，避免另一用例继承连接。
    def close(self):
        async def stop():
            self.server.close()
            await self.server.wait_closed()
            for writer, upstream in self.connections:
                writer.close()
                upstream.close()
            for upstream in self.limbo:
                upstream.close()
            tasks = [task for task in asyncio.all_tasks() if task is not asyncio.current_task()]
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)

        asyncio.run_coroutine_threadsafe(stop(), self.loop).result(timeout=8)
        self.loop.call_soon_threadsafe(self.loop.stop)
        self.thread.join(timeout=8)
        self.loop.close()


# 一个客户端独占音频 daemon、录音、MCP 与 user/net/mount/UTS namespace。
class Client:
    def __init__(self, world, role, credentials):
        self.world = world
        self.role = role
        self.app_pid = None
        self.app_birth = None
        self.process = None
        self.network = None
        self.samples = []
        world.clients.append(self)
        self.directory = world.directory / role
        self.directory.mkdir()
        for name in ("home", "state", "tmp", "runtime", "config", "cache", "data"):
            (self.directory / name).mkdir(mode=0o700)
        self.gate = Gate(world.server_port)
        world.gates.append(self.gate)
        self.name = f"rb-{world.directory.name[-8:]}-{role}"
        (self.directory / "hostname").write_text(self.name + "\n")
        runtime = world.runtime / role
        runtime.mkdir(mode=0o700)
        self.env = world.env | {
            "HOME": str(self.directory / "home"),
            "XDG_STATE_HOME": str(self.directory / "state"),
            "XDG_CONFIG_HOME": str(self.directory / "config"),
            "XDG_CACHE_HOME": str(self.directory / "cache"),
            "XDG_DATA_HOME": str(self.directory / "data"),
            "XDG_RUNTIME_DIR": str(runtime),
            "TMPDIR": str(self.directory / "tmp"),
            "DISPLAY": world.display,
            "SLINT_MCP_PORT": "8091",
            "RUST_LOG": "info,ui=debug",
        }
        bus = runtime / "bus"
        self.env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path={bus}"
        world.spawn(
            role + "-dbus",
            ["dbus-daemon", "--session", "--nofork", f"--address=unix:path={bus}"],
            self.env,
        )
        wait_until(bus.exists, role + " private D-Bus")
        self.socket = runtime / "pulse.sock"
        config = self.directory / "pulse.pa"
        config.write_text(
            f"load-module module-native-protocol-unix socket={self.socket} auth-anonymous=1\n"
            "load-module module-null-sink sink_name=rb rate=48000 channels=2\n"
            "set-default-sink rb\n"
        )
        world.spawn(
            role + "-pulse",
            [
                "pulseaudio",
                "-n",
                "--daemonize=no",
                "--exit-idle-time=-1",
                "--use-pid-file=no",
                "--disable-shm=yes",
                "--log-target=stderr",
                "--file",
                config,
            ],
            self.env,
        )
        self.pulse = f"unix:{self.socket}"
        self.env["PULSE_SERVER"] = self.pulse
        wait_until(self.pulse_ready, role + " private audio service")
        alsa = self.directory / "asound.conf"
        alsa.write_text(
            f'pcm.!default {{ type pulse server "{self.pulse}" }}\n'
            f'ctl.!default {{ type pulse server "{self.pulse}" }}\n'
        )
        self.env["ALSA_CONFIG_PATH"] = str(alsa)
        self.capture = self.directory / "output.s16le"
        capture_file = self.capture.open("wb")
        world.open_files.append(capture_file)
        self.recorder = world.spawn(
            role + "-capture",
            [
                "parec",
                "--server",
                self.pulse,
                "--device=rb.monitor",
                "--raw",
                "--format=s16le",
                "--rate=48000",
                "--channels=2",
                # 默认 monitor 以 2s 调度,与本轮 CPAL 短缓冲不匹配。
                f"--latency-msec={CAPTURE_LATENCY_MS}",
            ],
            self.env,
            stdout=capture_file,
        )
        self.start()
        from ui import UI

        self.ui = UI(self)
        self.ui.login(*credentials)

    # 真实音频服务启动失败不会被算成客户端静音。
    def pulse_ready(self):
        if not self.socket.exists():
            return False
        return (
            subprocess.run(
                ["pactl", "--server", self.pulse, "info"],
                capture_output=True,
                timeout=5,
                check=False,
            ).returncode
            == 0
        )

    # 与旧 ns-desktop 同形，mount 改写只发生于私有 namespace。
    def start(self):
        self.pid_file = self.directory / "app.pid"
        self.pid_file.unlink(missing_ok=True)
        ready = self.directory / "network.ready"
        ready.unlink(missing_ok=True)
        wrapper = (
            'mount --bind "$1" /etc/hostname; ip link set lo up; '
            'printf "%s\\n" "$$" > "$2"; '
            'while [ ! -f "$4" ]; do sleep 0.1; done; exec "$3"'
        )
        process = self.world.spawn(
            self.role + f"-app-{len(self.world.processes)}",
            [
                "unshare",
                "--user",
                "--map-root-user",
                "--net",
                "--mount",
                "--uts",
                "--",
                "sh",
                "-ec",
                wrapper,
                "rb-client",
                self.directory / "hostname",
                self.pid_file,
                self.world.desktop,
                ready,
            ],
            self.env,
        )
        self.process = process
        wait_until(lambda: process.alive() is None and self.pid_file.exists(), self.role + " PID")
        reported = int(self.pid_file.read_text())
        if reported != process.child.pid:
            raise RuntimeError("namespace wrapper PID is not the held command PID")
        self.app_pid = process.child.pid
        self.app_birth = self.identity()
        for namespace in ("net", "mnt", "uts", "user"):
            own = (Path("/proc/self/ns") / namespace).readlink()
            child = (Path("/proc") / str(self.app_pid) / "ns" / namespace).readlink()
            if own == child:
                raise RuntimeError(f"client lacks private {namespace} namespace")
        network = self.world.spawn(
            self.role + f"-network-{len(self.world.processes)}",
            [
                "pasta",
                "-f",
                "--config-net",
                "--ns-ifname",
                "rb0",
                "--netns",
                f"/proc/{self.app_pid}/ns/net",
                "--userns",
                f"/proc/{self.app_pid}/ns/user",
                "--host-lo-to-ns-lo",
                "-t",
                "none",
                "-u",
                "none",
                "-T",
                f"3000:{self.gate.port},{self.world.media.http.server_port}",
                "-U",
                "none",
            ],
            self.env,
        )
        self.network = network
        wait_until(
            lambda: (
                network.alive() is None
                and "rb0:" in Path(f"/proc/{self.app_pid}/net/dev").read_text()
            ),
            self.role + " network interface",
        )
        ready.touch()

    # PID 与内核启动 tick 配对，清理不能打到复用 PID 的无关进程。
    def identity(self):
        text = (Path("/proc") / str(self.app_pid) / "stat").read_text()
        return text.rsplit(")", 1)[1].split()[19]

    # 退出确认仍核对 birth；僵尸已停止执行，由自有父进程回收。
    def app_alive(self):
        try:
            text = (Path("/proc") / str(self.app_pid) / "stat").read_text()
        except FileNotFoundError:
            return False
        fields = text.rsplit(")", 1)[1].split()
        if fields[19] != self.app_birth:
            raise RuntimeError(f"client PID identity changed: {self.role}")
        return fields[0] != "Z"

    # 停精确应用后再停自己的 pasta，重启保留此客户端状态。
    def stop(self):
        if self.app_pid is not None:
            fd = None
            try:
                fd = os.pidfd_open(self.app_pid)
                if self.identity() != self.app_birth:
                    raise RuntimeError(f"client PID identity changed: {self.role}")
                signal.pidfd_send_signal(fd, signal.SIGTERM)
            except (FileNotFoundError, ProcessLookupError):
                pass
            finally:
                if fd is not None:
                    os.close(fd)
        if self.process:
            self.process.stop()
        if self.network:
            self.network.stop()
        if self.app_pid is not None:
            wait_until(lambda: not self.app_alive(), self.role + " app exit", timeout=8)
        self.app_pid = None

    # 重连由真实应用发起，MCP 句柄与窗口重新获取。
    def restart(self):
        self.stop()
        self.start()
        from ui import UI

        self.ui = UI(self)

    # 私有音频图绑定 app PID、实际 sink-input 与 monitor，原始图进入证据。
    def graph(self, require_stream):
        if self.process is None:
            raise RuntimeError("client has no live process handle")
        self.process.alive()
        self.recorder.alive()
        sinks = json.loads(
            subprocess.check_output(
                ["pactl", "--format=json", "--server", self.pulse, "list", "sinks"], timeout=5
            )
        )
        inputs = json.loads(
            subprocess.check_output(
                ["pactl", "--format=json", "--server", self.pulse, "list", "sink-inputs"], timeout=5
            )
        )
        sources = json.loads(
            subprocess.check_output(
                ["pactl", "--format=json", "--server", self.pulse, "list", "sources"], timeout=5
            )
        )
        captures = json.loads(
            subprocess.check_output(
                ["pactl", "--format=json", "--server", self.pulse, "list", "source-outputs"],
                timeout=5,
            )
        )
        graph = {
            "pid": self.app_pid,
            "birth": self.app_birth,
            "socket": str(self.socket),
            "sinks": sinks,
            "sink_inputs": inputs,
            "sources": sources,
            "source_outputs": captures,
        }
        with (self.directory / "audio-graphs.jsonl").open("a") as log:
            log.write(json.dumps(graph) + "\n")
        if (
            len(sinks) != 1
            or sinks[0]["name"] != "rb"
            or sinks[0]["monitor_source"] != "rb.monitor"
        ):
            raise RuntimeError("private sink topology changed")
        for item in inputs:
            if str(item["properties"].get("application.process.id")) != str(self.app_pid):
                raise RuntimeError(f"foreign process in private audio service: {item}")
            if item["sink"] != sinks[0]["index"]:
                raise RuntimeError("app stream does not target the captured sink")
        if require_stream and not inputs:
            raise RuntimeError("no app-owned output stream for audible PCM")
        monitors = [item for item in sources if item["name"] == "rb.monitor"]
        if len(monitors) != 1 or len(captures) != 1:
            raise RuntimeError("monitor capture topology changed")
        capture = captures[0]
        if capture["source"] != monitors[0]["index"] or str(
            capture["properties"].get("application.process.id")
        ) != str(self.recorder.child.pid):
            raise RuntimeError("capture is not the run-owned recorder on the private monitor")
        return graph

    # 每个窗口起止按字节位置记录；音频不足是环境错误。
    def window(self):
        self.recorder.alive()
        graph_before = self.graph(False)
        before = self.capture.stat().st_size // 4 * 4
        wanted = int(CAPTURE_RATE * PCM_WINDOW) * 4
        started = time.time()
        monotonic_started = time.monotonic()
        wait_until(
            lambda: (
                self.recorder.alive() is None and self.capture.stat().st_size >= before + wanted
            ),
            self.role + " PCM capture",
            timeout=8,
        )
        with self.capture.open("rb") as stream:
            stream.seek(before)
            pcm = stream.read(wanted)
        sample = {
            "role": self.role,
            "path": str(self.capture),
            "offset": before,
            "bytes": len(pcm),
            "start": started,
            "end": time.time(),
            "monotonic_start": monotonic_started,
            "monotonic_end": time.monotonic(),
            "graph_before": graph_before,
        }
        self.samples.append(sample)
        return pcm, sample

    # 设备身份由生产客户端生成，账号级隔离 SQL 仅用于观察。
    def device(self):
        path = self.directory / "state/osmosis-dev/device"
        return wait_until(
            lambda: path.read_text().strip() if path.exists() else None,
            self.role + " device identity",
        )


# 单一场景拥有自己的数据库、账号和三个真实客户端，可跨进程并行运行。
class World:
    def __init__(self, root, directory, desktop, server, media_duration=DURATION):
        self.root, self.directory = root, directory
        self.desktop, self.server_binary = desktop, server
        self.processes, self.gates, self.clients, self.open_files = [], [], [], []
        # socket不随持久证据目录嵌套,本轮独有短路径在关闭所有进程后回收。
        self.runtime_directory = tempfile.TemporaryDirectory(prefix="rb-")
        self.runtime = Path(self.runtime_directory.name)
        self.media = None
        self.media_duration = media_duration
        self.db: psycopg.Connection[dict[str, Any]] | None = None
        self.env = {
            key: value
            for key, value in os.environ.items()
            if key
            not in (
                "WAYLAND_DISPLAY",
                "DBUS_SESSION_BUS_ADDRESS",
                "OSMOSIS_API_BASE",
                "S3_ENDPOINT",
                "S3_ACCESS_KEY_ID",
                "S3_SECRET_ACCESS_KEY",
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
                "http_proxy",
                "https_proxy",
                "all_proxy",
                "PULSE_SERVER",
                "PIPEWIRE_REMOTE",
                "ALSA_CONFIG_PATH",
                "PULSE_SINK",
                "PULSE_SOURCE",
                "SLINT_LIVE_PREVIEW",
            )
        }
        self.env["WGPU_BACKEND"] = "vulkan"

    # 统一登记命令身份，部分初始化失败也能停止已起资源。
    def spawn(self, name, command, env=None, stdout=None, pass_fds=()):
        process = Process(self.directory, name, command, env or self.env, stdout, pass_fds)
        self.processes.append(process)
        return process

    # 数据库只监听独有 unix socket；迁移完全由真实 server 执行。
    def start(self):
        self.pg_data = self.directory / "pg-data"
        self.pg_socket = self.runtime / "pg"
        self.pg_socket.mkdir()
        initialization = self.spawn(
            "initdb",
            ["initdb", "-D", self.pg_data, "-A", "trust", "--no-locale", "--encoding=UTF8"],
        )
        try:
            status = initialization.child.wait(timeout=30)
            if status:
                raise RuntimeError(f"initdb exited: {status}")
        finally:
            initialization.stop()
        postgres = self.spawn(
            "postgres", ["postgres", "-D", self.pg_data, "-k", self.pg_socket, "-h", ""]
        )
        wait_until(
            lambda: (
                postgres.alive() is None
                and subprocess.run(
                    [
                        "pg_isready",
                        "--host",
                        str(self.pg_socket),
                        "--dbname",
                        "postgres",
                        "--timeout=1",
                    ],
                    capture_output=True,
                    timeout=5,
                    check=False,
                ).returncode
                == 0
            ),
            "private Postgres accepting connections",
        )
        with psycopg.connect(dbname="postgres", host=str(self.pg_socket), autocommit=True) as db:
            db.execute("CREATE DATABASE osmosis")
        self.db = psycopg.Connection[dict[str, Any]].connect(
            dbname="osmosis",
            host=str(self.pg_socket),
            autocommit=True,
            row_factory=psycopg.rows.dict_row,
        )
        self.media = Media(self.root, self.directory, self.media_duration)
        self.server_env = self.env | {
            "DATABASE_URL": f"postgresql://{quote(getpass.getuser())}@localhost/osmosis?host={quote(str(self.pg_socket))}",
            "BANG_DREAM_ADDR": f"http://127.0.0.1:{self.media.grpc_port}",
            "INVITE_CODE": self.directory.name,
            "BIND": "127.0.0.1:0",
            "RUST_LOG": "info,server=debug",
        }
        self.start_server()
        display_file = self.directory / "display"
        with display_file.open("wb") as output:
            self.spawn(
                "xvfb",
                [
                    "Xvfb",
                    "-displayfd",
                    str(output.fileno()),
                    "-screen",
                    "0",
                    "1100x900x24",
                    "-nolisten",
                    "tcp",
                    "-ac",
                ],
                pass_fds=(output.fileno(),),
            )
        wait_until(lambda: display_file.read_text().strip(), "Xvfb display")
        self.display = ":" + display_file.read_text().strip()
        self.username = self.directory.name
        self.password = self.directory.name + "-password"
        self.request(
            "/register",
            {"username": self.username, "password": self.password, "invite": self.directory.name},
        )
        self.account = self.sql("SELECT id FROM accounts WHERE username = %s", (self.username,))[0][
            "id"
        ]
        for role in ("controller", "one", "two"):
            Client(self, role, (self.username, self.password))
        self.controller, self.one, self.two = self.clients
        self.baseline_silence()
        return self

    # 健康检查每次读自己的实际绑定端口。
    def start_server(self):
        self.server_process = self.spawn(
            f"server-{len(self.processes)}", [self.server_binary], self.server_env
        )
        self.server_port = wait_until(lambda: listener_port(self.server_process), "server listener")
        wait_until(self.healthy, "server health")

    # 重启之后更新每道闸的上游端口，客户端仍自己重连。
    def restart_server(self):
        self.server_process.stop()
        self.start_server()
        for gate in self.gates:
            gate.loop.call_soon_threadsafe(setattr, gate, "server_port", self.server_port)

    # 探活拒绝错误响应，不能把跑着的别的服务认作本轮 server。
    def healthy(self):
        self.server_process.alive()
        try:
            self.request("/health")
            return True
        except (OSError, urllib.error.URLError):
            return False

    # 唯一直接 HTTP 写是账号初始化，遥控行为通过 UI。
    def request(self, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(
            f"http://127.0.0.1:{self.server_port}{path}",
            data=data,
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=8) as response:
            return json.load(response)

    # SQL 参数化且数据库独有，生命周期现场只在此库制造。
    def sql(self, statement, params=()):
        if self.db is None:
            raise RuntimeError("database is not initialized")
        cursor = self.db.execute(statement, params)
        return cursor.fetchall() if cursor.description else []

    # 每个场景从有效采集窗口证明三台均空闲。
    def baseline_silence(self):
        for client in self.clients:
            pcm, sample = client.window()
            quiet(pcm)
            sample["graph"] = client.graph(False)
            sample["baseline"] = "idle"

    # 接收端的可观测音频必须与 DB 时间线一致，DB 本身不代替 PCM。
    def sound(self, client, track_id, position=None, group=True, position_started=None):
        deadline = time.monotonic() + TIMEOUT
        last = None
        while time.monotonic() < deadline:
            pcm, sample = client.window()
            try:
                seconds = identify(pcm, track_id, self.media_duration)
                sample["graph"] = client.graph(True)
                sample["decoded_seconds"] = seconds
                if position is not None:
                    expected_position = position + max(0, sample["start"] - position_started)
                    if abs(seconds[0] - expected_position) > POSITION_TOLERANCE:
                        raise AssertionError(
                            f"audio: seek want {expected_position}s, got {seconds}"
                        )
                if group:
                    row = self.group()
                    expected = row["position_us"] / 1_000_000
                    if row["playing"]:
                        expected += max(0, sample["end"] - row["anchor_wall_us"] / 1_000_000)
                    if abs(seconds[-1] - expected) > POSITION_TOLERANCE:
                        raise AssertionError(f"audio: timeline want {expected}, got {seconds}")
                return Observation(
                    tuple(seconds), sample["monotonic_start"], sample["monotonic_end"]
                )
            except AssertionError as error:
                last = str(error)
        raise AudioFailure(f"audio: {client.role} did not output {track_id}: {last}")

    # 稳定静音判据也保留完整窗口和音频图。
    def silent(self, *clients):
        windows = []
        for client in clients:
            deadline = time.monotonic() + TIMEOUT
            last = None
            while time.monotonic() < deadline:
                pcm, sample = client.window()
                sample["graph"] = client.graph(False)
                try:
                    quiet(pcm)
                    windows.append(sample)
                    break
                except AssertionError as error:
                    last = str(error)
            else:
                raise AudioFailure(f"audio: {client.role} did not become silent: {last}")
        return windows

    # 从真实音频建立十二秒非零且至少推进两秒的前提，拒绝起播瞬间的弱比较。
    def progressed(self, client, track, group=True):
        first = self.sound(client, track, group=group)
        deadline = time.monotonic() + TIMEOUT
        while time.monotonic() < deadline:
            observed = self.sound(client, track, group=group)
            if observed[0] >= MIN_POSITION and observed[-1] >= first[-1] + MIN_ADVANCE:
                self.continued(first, observed)
                return observed
        raise AudioFailure(f"audio: {client.role} never established nonzero advancing PCM")

    # 暂停保持期间每个新窗口都必须有效静音，不以任意 sleep 建立冻结前提。
    def held_silent(self, client):
        first = None
        while True:
            pcm, sample = client.window()
            sample["graph"] = client.graph(False)
            try:
                quiet(pcm)
            except AssertionError as error:
                raise AudioFailure(f"audio: paused output resumed during hold: {error}") from error
            if first is None:
                first = sample["monotonic_start"]
            if sample["monotonic_end"] - first >= PAUSE_HOLD:
                return

    # 音频接续断言不重试；错误回零/前跳不能靠继续等待变成通过。
    def continued(self, before, after, action=None, pause=None):
        bounds = continuation(before, after) if pause is None else resumed(before, after, *pause)
        record = {
            "before": asdict(before),
            "after": asdict(after),
            "bounds": bounds,
            "tolerance": POSITION_TOLERANCE,
            "action": None if action is None else asdict(action),
            "pause": None
            if pause is None
            else {
                "requested": asdict(pause[0]),
                "first_silent": pause[1],
                "resume": asdict(pause[2]),
            },
        }
        try:
            check(before, after, bounds, action)
        except AssertionError as error:
            record["failure"] = str(error)
            raise AudioFailure(str(error)) from error
        finally:
            with (self.directory / "continuity.jsonl").open("a") as log:
                log.write(json.dumps(record) + "\n")

    # 唯一账号的组状态只能属于本场景。
    def group(self):
        rows = self.sql("SELECT * FROM play_groups WHERE account_id = %s", (self.account,))
        with (self.directory / "group-observations.jsonl").open("a") as log:
            log.write(json.dumps({"time": time.time(), "rows_repr": repr(rows)}) + "\n")
        return rows[0] if rows else None

    # 不删除录音和日志；只停止本轮登记的客户端、服务与数据库。
    def close(self):
        errors = []

        # 一个清理动作失败不能阻断余下资源回收，最终统一报错并保留原异常类别。
        def finish(label, action):
            try:
                action()
            except (
                OSError,
                RuntimeError,
                TimeoutError,
                ValueError,
                psycopg.Error,
                subprocess.SubprocessError,
            ) as error:
                errors.append(f"{label}: {type(error).__name__}: {error}")

        for resource in [*reversed(self.clients), *reversed(self.gates)]:
            finish(
                type(resource).__name__,
                resource.stop if isinstance(resource, Client) else resource.close,
            )
        if self.db:
            finish("database connection", self.db.close)
        if self.media:
            finish("media service", self.media.close)
        for process in reversed(self.processes):
            finish(process.name, process.stop)
        for stream in self.open_files:
            finish(str(stream.name), stream.close)
        finish("private runtime directory", self.runtime_directory.cleanup)
        (self.directory / "resources.json").write_text(
            json.dumps(
                {
                    "processes": [process.receipt() for process in self.processes],
                    "audio_windows": [
                        sample for client in self.clients for sample in client.samples
                    ],
                    "runtime_directory": str(self.runtime),
                    "runtime_removed": not self.runtime.exists(),
                    "cleanup_errors": errors,
                },
                indent=2,
            )
        )
        if errors:
            raise RuntimeError(f"incomplete cleanup: {errors}")
