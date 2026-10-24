# test/headless —— 把桌面实例开在看不见的显示里

负责:起一个无头 sway(借 GPU 渲染节点合成,wgpu 纹理是真的),把 `osmosis-desktop` 放进
pasta 的独立网络命名空间里连上它,用 grim 抓像素。用户的 niri 桌面上不出现窗口、不抢焦点,
声音进 `test/null-sink.ts` 的 null sink,可以和用户桌面上的开发实例并存,也能无人值守。

`just shot`、`just desktop-dev`、`test/mcp-e2e` 的测试夹具都经这里起实例。

不负责:应用本身的渲染;像素比较(在 `test/mcp-e2e/visual.py`);构建二进制(调用方先编)。

依赖:`slint.nix` 提供的 sway、pasta(passt)、grim 和应用的动态库,所以要在
`nix-shell slint.nix` 里跑;PipeWire;一块可读写的 `/dev/dri/renderD*`(自动挑非 nvidia 的,
`OSMOSIS_RENDER_NODE` 可覆盖)。

```sh
bun test/headless/headless.ts shot --out dist/shot.png --width 420 --height 900 --dest 8
bun test/headless/headless.ts run --dir <目录> --mcp-port 8091   # 前台常驻,SIGTERM 收尾
bun test/headless/headless.ts capture --dir <目录> --out a.png    # 从常驻会话抓一张
nix-shell slint.nix --run 'bun test/headless/check.ts'           # AC 检查,真起一遍
```

为什么这么拼:niri 只给可见窗口发 frame callback,不聚焦就截到旧画面;应用的单实例锁是
abstract socket,按网络命名空间区分,换一个命名空间才起得了第二个。命名空间里的
`127.0.0.1:3000` 由 pasta 接到宿主的本机后端(`--backend-port` 指宿主端口,缺省 3000;测试夹具
给自己那份 server 的随机端口,宿主 3000 被别人占着也不碍事),MCP 端口用 `--mcp-port` 转发出来。应用要直连的其他宿主回环端口(如夹具的媒体替身)用
`--forward P1,P2` 接进命名空间,同号映射。

登录态:会话目录里放一份 `~/.local/state/osmosis-dev` 的副本当 `XDG_STATE_HOME`,不写回。

`check.ts` 判的是:截图前后 niri 焦点与窗口集合不变、应用没有连到真实输出的音频链路、
截图中央区不是纯色(GPU 纹理在)、已有一个实例常驻时第二个照样起得来且前者不受影响。
