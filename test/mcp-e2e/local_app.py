"""只替换外部音乐提供方；真实桌面、生产 server 与数据库走完整用户路径。"""

import importlib.util
import json
import os
import shlex
import signal
import socket
import subprocess
import time
import urllib.error
import urllib.request
import uuid
from contextlib import ExitStack
from pathlib import Path

from mcp_client import Client, McpError
from pick import sql


class Resources:
    """每轮拥有独立容器和进程组，部分初始化失败也按身份清理。"""

    def __init__(self, directory: Path):
        """只生成随机身份；构造本身不启动服务。"""
        self.directory = directory
        directory.mkdir(parents=True, exist_ok=True)
        self.container = "osmosis-mcp-" + uuid.uuid4().hex
        self.stack = ExitStack()

    def __enter__(self):
        """进入本轮资源作用域。"""
        return self

    def __exit__(self, *arguments):
        """逆序结束应用、服务、媒体与容器。"""
        return self.stack.__exit__(*arguments)

    def spawn(self, name: str, command: list[str], env=None):
        """持有真实 Popen 句柄；日志不记录环境或命令中的临时凭据。"""
        log = self.stack.enter_context((self.directory / f"{name}.log").open("wb"))
        process = subprocess.Popen(
            command, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True
        )
        self.stack.callback(self.stop, process, name)
        return process

    def stop(self, process, name):
        """结束本轮独占进程组，并通过原父进程读取退出状态。"""
        natural_exit = process.poll()
        (self.directory / f"{name}-exit.json").write_text(
            json.dumps({"pid": process.pid, "exit_before_cleanup": natural_exit})
        )
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)

    def database(self):
        """Postgres 使用随机容器名和宿主端口，不挂真实数据卷。"""
        self.stack.callback(self.remove_database)
        subprocess.run(
            [
                "docker",
                "run",
                "--pull=never",
                "-d",
                "--name",
                self.container,
                "-e",
                "POSTGRES_USER=slint",
                "-e",
                "POSTGRES_DB=osmosis",
                "-e",
                f"POSTGRES_PASSWORD={self.container}",
                "-p",
                "127.0.0.1::5432",
                "postgres:17-alpine",
            ],
            check=True,
            capture_output=True,
            timeout=30,
        )
        port = (
            subprocess.run(
                ["docker", "port", self.container, "5432/tcp"],
                check=True,
                capture_output=True,
                text=True,
            )
            .stdout.strip()
            .rsplit(":", 1)[1]
        )
        return f"postgres://slint:{self.container}@127.0.0.1:{port}/osmosis"

    def remove_database(self):
        """只移除本对象生成的容器和它的匿名卷。"""
        result = subprocess.run(
            ["docker", "rm", "-fv", self.container],
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
        if result.returncode and "no such" not in result.stderr.lower():
            raise subprocess.CalledProcessError(
                result.returncode, result.args, stderr=result.stderr
            )


def wait_ready(probe, process=None, timeout=60):
    """等待本轮资源就绪，进程提前退出立即报错。"""
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        if process is not None and process.poll() is not None:
            raise RuntimeError(f"owned process exited: {process.returncode}")
        try:
            result = probe()
            if result:
                return result
        except (OSError, McpError, subprocess.SubprocessError) as error:
            last = error
        time.sleep(0.2)
    raise AssertionError(f"resource not ready: {last}")


def require_free(port: int):
    """编译期固定的本机后端端口必须独占，不能误连别的会话。"""
    with socket.socket() as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", port))


def media_class(root: Path):
    """复用原有确定音源，运行系统及真实业务层不作替换。"""
    spec = importlib.util.spec_from_file_location(
        "osmosis_fixture_media", root / "test/remote-behavior/media.py"
    )
    if spec is None or spec.loader is None:
        raise RuntimeError("media fixture loader unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.Media


class LocalApp:
    """独立账号、数据库及状态目录的真实桌面环境。"""

    def __init__(self, directory: Path):
        """由调用者分配证据目录，所有可变资源属于本轮。"""
        self.directory = directory
        self.root = Path(__file__).resolve().parents[2]
        self.resources = Resources(directory)
        self.state = directory / "state"
        self.username = self.resources.container
        self.environment = {"PG_CONTAINER": self.resources.container, "PG_DB": "osmosis"}
        self.client = Client(timeout=60)
        self.http = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def request(self, path: str, body=None):
        """HTTP 只用于本轮数据准备，不替代被测的点击和播放。"""
        request = urllib.request.Request(
            "http://127.0.0.1:3000" + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        with self.http.open(request, timeout=5) as response:
            return json.load(response)

    def sql(self, statement: str):
        """查询只指向对象自己的数据库，不读取默认开发容器。"""
        return sql(self.environment, statement)

    def start(self):
        """准备隔离数据后经 MCP 登录、进入音乐页。"""
        require_free(3000)
        require_free(8091)
        database_url = self.resources.database()
        wait_ready(lambda: self.sql("select 1") == "1")
        media = media_class(self.root)(self.root, self.directory)
        self.resources.stack.callback(media.close)
        # 不向临时子进程传入真实凭据、持久状态或生产地址覆盖。
        env = {
            key: value
            for key, value in os.environ.items()
            if key
            not in {
                "OSMOSIS_API_BASE",
                "TEST_USERNAME",
                "TEST_PASSWORD",
                "ANDROID_DEVICE_PIN",
                "S3_ENDPOINT",
                "S3_ACCESS_KEY_ID",
                "S3_SECRET_ACCESS_KEY",
                "SLINT_LIVE_PREVIEW",
            }
        }
        env.update(
            DATABASE_URL=database_url,
            BANG_DREAM_ADDR=f"http://127.0.0.1:{media.grpc_port}",
            BIND="127.0.0.1:3000",
            INVITE_CODE=self.username,
            XDG_STATE_HOME=str(self.state),
            XDG_CACHE_HOME=str(self.directory / "cache"),
            XDG_CONFIG_HOME=str(self.directory / "config"),
            SLINT_MCP_PORT="8091",
        )
        server = self.resources.spawn("server", [str(self.root / "target/debug/server")], env)
        wait_ready(lambda: self.request("/health"), server)
        self.request(
            "/register",
            {"username": self.username, "password": self.username, "invite": self.username},
        )
        self.seed_playlists()
        app = self.resources.spawn(
            "desktop",
            [
                "nix-shell",
                str(self.root / "slint.nix"),
                "--run",
                "exec " + shlex.quote(str(self.root / "target/debug/osmosis-desktop")),
            ],
            env,
        )
        wait_ready(
            lambda: (
                "Slint MCP server listening"
                in (self.directory / "desktop.log").read_text(errors="replace")
            ),
            app,
        )
        wait_ready(lambda: self.client.by_id("LoginPage::username").all(), app)
        for element_id in ("LoginPage::username", "LoginPage::password"):
            self.client.call(
                "set_element_value",
                elementHandle=self.client.by_id(element_id).handle(),
                value=self.username,
            )
        self.client.by_label("登录", role="Button").click()
        wait_ready(lambda: not self.client.by_id("LoginPage::username").all(), app)
        # 仅初始化使用现有壳入口；样板自身从音乐页起步，不依赖 Home。
        if not self.client.by_label("每日推荐").all():
            self.client.by_label("音乐", role="Button").click()
        self.client.by_label("每日推荐").handle()
        return self

    def seed_playlists(self):
        """在独有数据库准备两个互不相交的集合，业务读取仍走生产 API。"""
        self.sql("""INSERT INTO local_playlists (account_id,name,system)
            SELECT id,'我的喜欢','liked' FROM accounts;
            INSERT INTO local_playlists (account_id,name)
            SELECT id,'MCP alternative' FROM accounts;
            INSERT INTO local_playlist_tracks (playlist_id,platform,track_id,position)
            SELECT id,'netease',CASE WHEN system='liked' THEN '175004' ELSE '175006' END,0
            FROM local_playlists;
            INSERT INTO local_playlist_tracks (playlist_id,platform,track_id,position)
            SELECT id,'netease',CASE WHEN system='liked' THEN '175005' ELSE '175007' END,1
            FROM local_playlists;""")
