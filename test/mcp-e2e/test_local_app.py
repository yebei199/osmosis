"""本地运行 fixture 的资源身份、异常退出和数据隔离合同。"""

import sys

import pytest

from local_app import Resources
from mcp_client import expect


def test_process_is_reaped_when_setup_fails(tmp_path):
    """部分初始化失败也必须结束本轮拥有的进程，不按名称杀其他任务。"""
    with pytest.raises(RuntimeError, match="setup fault"), Resources(tmp_path) as resources:
        process = resources.spawn("child", [sys.executable, "-c", "import time; time.sleep(60)"])
        assert process.poll() is None
        raise RuntimeError("setup fault")
    assert process.poll() is not None


def test_independent_runs_have_distinct_container_names(tmp_path):
    """资源名绑定运行身份，禁止复用真实开发库容器名。"""
    with Resources(tmp_path / "one") as one, Resources(tmp_path / "two") as two:
        assert one.container != two.container
        assert one.container.startswith("osmosis-mcp-")
        assert two.container != "osmosis-pg"


@pytest.mark.app
def test_database_and_state_belong_to_this_run(local_app):
    """实际容器、账号与状态目录独立，fixture 不写真实账号或默认状态树。"""
    from pathlib import Path

    environment = Path(f"/proc/{local_app.app_process.pid}/environ").read_bytes().split(b"\0")
    assert b"PIPEWIRE_NODE=" + local_app.audio_sink.encode() in environment
    assert local_app.resources.sink_id(local_app.audio_sink) is not None
    assert local_app.environment["PG_CONTAINER"] == local_app.resources.container
    assert local_app.environment["PG_CONTAINER"] != "osmosis-pg"
    assert local_app.sql("select count(*) from accounts") == "1"
    assert local_app.sql("select username from accounts") == local_app.username
    assert local_app.state.is_relative_to(local_app.directory)
    expect(lambda: local_app.state.exists())


def test_database_is_removed_when_setup_fails(tmp_path):
    """实际独占容器在部分初始化失败时被删除，不留下匿名卷或容器。"""
    import subprocess

    with pytest.raises(RuntimeError, match="after database"), Resources(tmp_path) as resources:
        resources.database()
        raise RuntimeError("after database")
    result = subprocess.run(
        ["docker", "inspect", resources.container], capture_output=True, text=True, check=False
    )
    assert result.returncode != 0
    assert "no such" in result.stderr.lower()


def test_failed_container_creation_keeps_original_error(tmp_path, monkeypatch):
    """镜像不可用时不能让不存在容器的清理错误覆盖启动原因。"""
    import subprocess

    def docker_failure(command, **options):
        """只模拟 Docker 的失败边界，资源管理仍运行真实代码。"""
        if command[1] == "run":
            raise subprocess.CalledProcessError(125, command, stderr="image unavailable")
        if options.get("check"):
            raise subprocess.CalledProcessError(1, command, stderr="No such container")
        return subprocess.CompletedProcess(command, 1, stderr="No such container")

    monkeypatch.setattr(subprocess, "run", docker_failure)
    with pytest.raises(subprocess.CalledProcessError) as failure, Resources(tmp_path) as resources:
        resources.database()
    assert failure.value.returncode == 125


def test_port_guard_refuses_live_listener_but_allows_closed_connections():
    """固定应用端口不能抢占，上一轮 TIME_WAIT 也不能被误判为监听者。"""
    import socket

    from local_app import require_free

    with socket.socket() as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        port = listener.getsockname()[1]
        with pytest.raises(OSError):
            require_free(port)
        with socket.create_connection(("127.0.0.1", port), timeout=2) as client:
            connection, _ = listener.accept()
            connection.close()
            assert client.recv(1) == b""
    require_free(port)


def test_virtual_sink_is_owned_and_removed_on_failure(tmp_path):
    """每轮创建独有的真实 null sink，异常退出也必须销毁它。"""
    with pytest.raises(RuntimeError, match="after sink"), Resources(tmp_path) as resources:
        sink = resources.virtual_audio()
        assert resources.sink_id(sink) is not None
        raise RuntimeError("after sink")
    assert resources.sink_id(sink) is None
