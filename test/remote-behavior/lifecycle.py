"""父端持有独占 guardian IPC；命令超时和取消都先清理再回报。"""

import argparse
import json
import os
import select
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path


class OwnedCommand:
    def __init__(
        self, command, receipt, *, cwd=None, env=None, stdout=None, stderr=None, pass_fds=()
    ):
        self.command = list(map(str, command))
        self.returncode = None
        self.result = None
        self.control, child = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        try:
            self.guardian = subprocess.Popen(
                [sys.executable, str(Path(__file__).with_name("guardian.py")), str(child.fileno())],
                stdin=subprocess.DEVNULL,
                stdout=stdout,
                stderr=stderr,
                start_new_session=True,
                pass_fds=(child.fileno(), *pass_fds),
            )
        except OSError:
            self.control.close()
            raise
        finally:
            child.close()
        try:
            self.control.send(
                json.dumps(
                    {
                        "command": self.command,
                        "cwd": str(cwd) if cwd else None,
                        "env": env,
                        "pass_fds": list(pass_fds),
                        "receipt": str(receipt),
                    }
                ).encode()
            )
            event = self.receive(10)
            if event["event"] != "started":
                raise RuntimeError(f"guardian initialization failed: {event}")
            self.pid = event["pid"]
        except (OSError, TimeoutError, RuntimeError):
            self.control.close()
            self.guardian.wait(timeout=10)
            raise

    def receive(self, timeout):
        ready, _, _ = select.select([self.control], [], [], timeout)
        if not ready:
            raise TimeoutError("guardian IPC did not complete")
        packet = self.control.recv(65536)
        if not packet:
            raise RuntimeError(f"guardian disconnected: {self.guardian.poll()}")
        return json.loads(packet)

    def poll(self):
        while select.select([self.control], [], [], 0)[0]:
            event = self.receive(0)
            if event["event"] == "exit":
                self.returncode = event["status"]
            else:
                raise RuntimeError(f"unexpected guardian event: {event}")
        return self.returncode

    def wait(self, timeout):
        deadline = time.monotonic() + timeout
        while self.poll() is None:
            if time.monotonic() >= deadline:
                raise subprocess.TimeoutExpired(self.command, timeout)
            time.sleep(0.05)
        return self.returncode

    def stop(self):
        if self.result is not None:
            return
        try:
            self.control.send(b"close")
            while True:
                event = self.receive(12)
                if event["event"] == "exit":
                    self.returncode = event["status"]
                elif event["event"] == "closed":
                    self.result = event["result"]
                    self.returncode = self.result["exit"]
                    break
                else:
                    raise RuntimeError(f"guardian cleanup failed: {event}")
            status = self.guardian.wait(timeout=2)
            if status or not self.result["cleanup"]["complete"]:
                raise RuntimeError(f"guardian incomplete cleanup: {self.result}")
        finally:
            self.control.close()


# 每条嵌套命令都留原始状态和清理结果；124/130/143 不得当作变异 RED。
def run_logged(command, cwd, env, directory, name, timeout, cancel=None):
    started = time.time()
    status = None
    owner = None
    reason = "exit"
    try:
        with (directory / f"{name}.log").open("wb") as log:
            owner = OwnedCommand(
                command,
                directory / f"{name}-cleanup.json",
                cwd=cwd,
                env=env,
                stdout=log,
                stderr=log,
            )
            deadline = time.monotonic() + timeout
            while (status := owner.poll()) is None:
                cancellation = cancel() if cancel else None
                if cancellation is not None:
                    status, reason = cancellation, "cancelled"
                    break
                if time.monotonic() >= deadline:
                    status, reason = 124, "timeout"
                    break
                time.sleep(0.05)
    finally:
        try:
            if owner is not None:
                owner.stop()
        finally:
            record = {
                "command": list(map(str, command)),
                "start": started,
                "end": time.time(),
                "exit": status,
                "reason": reason,
                "cleanup": None if owner is None else owner.result,
            }
            (directory / f"{name}.json").write_text(json.dumps(record, indent=2))
    return status


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--timeout", type=float, required=True)
    parser.add_argument("--cancel", type=Path, required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    requested = []

    def on_signal(number, _frame):
        requested.append(128 + number)

    signal.signal(signal.SIGTERM, on_signal)
    signal.signal(signal.SIGINT, on_signal)

    def cancelled():
        if requested:
            return requested[0]
        return int(args.cancel.read_text()) if args.cancel.exists() else None

    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("command is required")
    return run_logged(
        command, Path.cwd(), dict(os.environ), args.directory, args.name, args.timeout, cancelled
    )


if __name__ == "__main__":
    sys.exit(main())
