"""在目标客户端自己的 network namespace 内执行一次 MCP JSON-RPC。"""

import json
import sys
import urllib.request


# stdin/stdout 桥接只传 MCP 数据，不把 UI 回调换成直接业务函数。
def main():
    request = urllib.request.Request(
        "http://127.0.0.1:8091/mcp",
        data=sys.stdin.buffer.read(),
        headers={
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
        },
    )
    with urllib.request.urlopen(request, timeout=8) as response:
        result = json.load(response)
    print(json.dumps(result))


if __name__ == "__main__":
    main()
