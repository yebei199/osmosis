#!/usr/bin/env python3
"""一道只管 `/signal` 的闸:HTTP 照常放行,信令可以随时掐断、拒进(#118)。

真机上模拟「信令断了、而 HTTP 还通」:点歌要经服务端取直链,整条网都断的话
本机本来就放不了歌,测不出锁有没有撤。移动网络换线、中间盒掐掉长连接、
建连被限流,都是这个形状 —— 信令没了,HTTP 还在。

    test/signal-gate.py <监听端口> <上游端口> [上游主机,缺省 127.0.0.1]

    kill -USR1 <pid>   掐断:此刻所有 /signal 连接的客户端那一半断开,之后的
                       /signal 一进来就关。服务端那一半留着不关 —— 服务端要等
                       探活才发现,与移动网络掉线一样。
    kill -USR2 <pid>   恢复:/signal 重新放行。

每台设备走自己的一道闸(各占一个端口),才掐得了其中一台。
"""

import asyncio
import signal
import sys

blocked = False
# 正在转发的 /signal 连接:(客户端写端, 服务端写端)
live: set = set()
# 掐断时收下的服务端那一半,进程退出前不关。
limbo: list = []


async def pump(reader, writer):
    try:
        while data := await reader.read(65536):
            writer.write(data)
            await writer.drain()
    except (ConnectionError, OSError):
        pass
    finally:
        try:
            writer.close()
        except OSError:
            pass


async def serve(client_r, client_w, upstream):
    head = await client_r.read(65536)
    is_signal = head.startswith(b"GET /signal")
    if is_signal and blocked:
        client_w.close()
        return
    try:
        server_r, server_w = await asyncio.open_connection(*upstream)
    except OSError:
        client_w.close()
        return
    server_w.write(head)
    pair = (client_w, server_w)
    if is_signal:
        live.add(pair)
    asyncio.ensure_future(pump(server_r, client_w))
    if is_signal:
        await pump_keep(client_r, server_w, pair)
        live.discard(pair)
    else:
        await pump(client_r, server_w)


async def pump_keep(reader, writer, pair):
    """信令的上行:客户端那头断了不关服务端那一半 —— 掐断时要留它半开。"""
    try:
        while data := await reader.read(65536):
            writer.write(data)
            await writer.drain()
    except (ConnectionError, OSError):
        pass
    if pair in live:
        # 客户端自己走的(不是被掐的):照常关掉,服务端立刻知道。
        writer.close()


def cut():
    global blocked
    blocked = True
    for client_w, server_w in list(live):
        live.discard((client_w, server_w))
        limbo.append(server_w)
        client_w.transport.abort()
    print("gate: 掐断信令", flush=True)


def heal():
    global blocked
    blocked = False
    print("gate: 恢复信令", flush=True)


async def main():
    listen_port = int(sys.argv[1])
    upstream = (sys.argv[3] if len(sys.argv) > 3 else "127.0.0.1", int(sys.argv[2]))
    loop = asyncio.get_running_loop()
    loop.add_signal_handler(signal.SIGUSR1, cut)
    loop.add_signal_handler(signal.SIGUSR2, heal)
    server = await asyncio.start_server(
        lambda r, w: serve(r, w, upstream), "127.0.0.1", listen_port
    )
    print(f"gate: {listen_port} -> {upstream[0]}:{upstream[1]}", flush=True)
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
