"""Linux 子收割器持有后代身份；父端断开后仍负责收口自己的进程树。"""

import ctypes
import json
import os
import select
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

GRACE = 2
PR_SET_CHILD_SUBREAPER = 36


# 只枚举持有树的 children 文件，不搜索机器上的进程名或命令行。
def children(pid):
    try:
        return [
            int(value) for value in Path(f"/proc/{pid}/task/{pid}/children").read_text().split()
        ]
    except FileNotFoundError:
        return []


# proc stat 的 comm 可含空格及括号，字段从最后一个右括号之后计数。
def identity(pid):
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return int(fields[1]), int(fields[19])
    except FileNotFoundError:
        return None


# 先核对实际父子关系，再用 pidfd 固定出生身份；复核失败不发信号。
def acquire(pid, parent):
    before = identity(pid)
    if before is None or before[0] != parent:
        return None
    try:
        fd = os.pidfd_open(pid)
    except ProcessLookupError:
        return None
    if identity(pid) != before:
        os.close(fd)
        return None
    return fd, before[1]


# leader 退出后后代重托给本收割器；重复发现直至实际 children 为空。
class Custody:
    def __init__(self):
        self.handles = {}
        self.events = []

    def owns_parent(self, parent):
        if parent == os.getpid():
            return True
        handle = self.handles.get(parent)
        current = identity(parent)
        return handle is not None and current is not None and current[1] == handle[1]

    def discover(self, parent, freeze=False):
        if not self.owns_parent(parent):
            return
        for pid in children(parent):
            handle = self.handles.get(pid)
            current = identity(pid)
            if handle is not None and (current is None or current[1] != handle[1]):
                os.close(handle[0])
                self.handles.pop(pid)
                handle = None
            if handle is None:
                handle = acquire(pid, parent)
                if handle is None:
                    continue
                if not self.owns_parent(parent):
                    os.close(handle[0])
                    return
                self.handles[pid] = handle
            if freeze:
                self.send(pid, signal.SIGSTOP)
            self.discover(pid, freeze)

    def send(self, pid, sig):
        fd, birth = self.handles[pid]
        try:
            signal.pidfd_send_signal(fd, sig)
            self.events.append({"pid": pid, "birth": birth, "signal": int(sig)})
        except ProcessLookupError:
            pass

    def reap(self, command_pid):
        for child in children(os.getpid()):
            if child == command_pid:
                continue
            try:
                pid, _ = os.waitpid(child, os.WNOHANG)
            except ChildProcessError:
                continue
            if not pid:
                continue
            handle = self.handles.pop(pid, None)
            if handle is not None:
                os.close(handle[0])

    def cleanup(self, command):
        self.discover(os.getpid())
        for pid in self.handles:
            self.send(pid, signal.SIGTERM)
        deadline = time.monotonic() + GRACE
        while children(os.getpid()) and time.monotonic() < deadline:
            command.poll()
            self.reap(command.pid)
            time.sleep(0.05)
        deadline = time.monotonic() + GRACE * 2
        while children(os.getpid()) and time.monotonic() < deadline:
            # 先冻结父再发现其子，阻止清理时继续 fork；逃离 session 也仍属此树。
            self.discover(os.getpid(), freeze=True)
            for pid in reversed(self.handles):
                self.send(pid, signal.SIGKILL)
            command.poll()
            self.reap(command.pid)
            time.sleep(0.05)
        remaining = children(os.getpid())
        for fd, _ in self.handles.values():
            os.close(fd)
        return {"complete": not remaining, "remaining": remaining, "signals": self.events}


def send(control, payload):
    try:
        control.send(json.dumps(payload).encode())
    except (BrokenPipeError, ConnectionResetError):
        pass


def supervise(control, request, receipt, cancelled):
    command = subprocess.Popen(
        request["command"],
        cwd=request["cwd"],
        env=request["env"],
        stdin=subprocess.DEVNULL,
        pass_fds=tuple(request["pass_fds"]),
    )
    send(control, {"event": "started", "pid": command.pid})
    status = None
    try:
        while True:
            if cancelled:
                break
            observed = command.poll()
            if observed is not None and status is None:
                status = observed
                send(control, {"event": "exit", "status": status})
            ready, _, _ = select.select([control], [], [], 0.05)
            if ready:
                # close 请求和父进程消失均结束生命周期，不能仅等待 leader。
                control.recv(65536)
                break
    finally:
        cleanup = Custody().cleanup(command)
        result = {
            "command": request["command"],
            "pid": command.pid,
            "exit": status if status is not None else command.returncode,
            "cleanup": cleanup,
            "end": time.time(),
        }
        receipt.write_text(json.dumps(result, indent=2))
        send(control, {"event": "closed", "result": result})
    return 0 if cleanup["complete"] else 2


def main():
    # libc 仅设置本进程，失败即环境错误，禁止退化成 killpg。
    if ctypes.CDLL(None, use_errno=True).prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "cannot become child subreaper")
    control = socket.socket(fileno=int(sys.argv[1]))
    request = json.loads(control.recv(65536))
    receipt = Path(request["receipt"])
    cancelled = []

    def on_signal(number, _frame):
        cancelled.append(number)

    signal.signal(signal.SIGTERM, on_signal)
    signal.signal(signal.SIGINT, on_signal)
    try:
        return supervise(control, request, receipt, cancelled)
    except (OSError, ValueError) as error:
        receipt.write_text(json.dumps({"error": str(error), "cleanup": {"complete": False}}))
        send(control, {"event": "error", "error": str(error)})
        raise


if __name__ == "__main__":
    sys.exit(main())
