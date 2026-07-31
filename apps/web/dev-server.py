#!/usr/bin/env python3
"""dev 静态服务器:等同 `python -m http.server`,但禁止浏览器缓存。

存在的理由:标准库那个不发 `Cache-Control`,只发 `Last-Modified`。这种响应浏览器会按
「启发式新鲜度」自己决定缓存多久 —— 于是重新构建后页面仍在跑旧的 wasm。而旧 wasm 的
症状(3D 面板全黑、整个界面卡住不重绘)和「代码有 bug」在画面上完全无法区分,排查会
直接跑偏,我们已经为此绕过一次远路。

指望硬刷新绕开是不行的:Slint 的 canvas 吃掉键盘事件,`Ctrl+Shift+R` 到不了浏览器;
产物又有 36MB,肉眼也看不出新旧。所以由服务器一侧断掉缓存,而不是靠人记得刷新。

用法:dev-server.py <端口> <目录>
"""

import functools
import http.server
import sys


class NoCacheHandler(http.server.SimpleHTTPRequestHandler):
    """每个响应都带 no-store,浏览器每次都得重新取。"""

    def end_headers(self) -> None:
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


def main() -> None:
    port, directory = int(sys.argv[1]), sys.argv[2]
    handler = functools.partial(NoCacheHandler, directory=directory)
    # 绑回环:浏览器的安全上下文白名单只认 localhost / 127.0.0.1,从 0.0.0.0 打开
    # 拿不到 WebGPU(理由见 justfile 里 web-dev 的注释)。
    http.server.test(HandlerClass=handler, port=port, bind="127.0.0.1")


if __name__ == "__main__":
    main()
