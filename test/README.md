# test —— 端到端脚本与浏览器侧的对照页

## mcp-login.sh —— 把界面登进去

端到端脚本(如 `pick-e2e.sh`)把「已登录」写成前提却没人负责满足它,于是每次跑之前都要人手点一遍。
这份脚本补上那一步,可重复跑(已登录时直接返回)。

```sh
test/mcp-login.sh           # 安卓默认 8090;桌面用 PORT=8091 test/mcp-login.sh
```

凭据从 `.env` 读进 shell 变量再交给 `set_element_value`,**不进命令行、不打印**。
判据是登录页从元素树里消失,不是看画面。

它第一次跑就体现了价值:失败信息把责任定在「连不上服务端」,而不是脚本自己。

## dev-recipes.sh —— debug 配方的守卫还拦得住吗

```sh
test/dev-recipes.sh
```

`desktop-dev` / `mcp-android` 的本机后端守卫、`dev-adb` 的序列号解析、生产平板拒装
(#119)全是 justfile 里的 shell。这份把假的 `adb` 与 `nix-shell` 顶在 PATH 前面驱动
那几条配方,断言退出码、调用参数,以及守卫失败时**没有进编译**。不碰真设备,几秒跑完。
`desktop-install` 装的是带库路径的启动脚本而不是软链(#112),也在这里断言。
改了那几条配方就跑一遍。

## desktop-clean-launch.sh —— 菜单项离开 nix shell 还起得来吗(#112)

```sh
just desktop-install && test/desktop-clean-launch.sh
# 隔离目录里验,不碰真实的菜单项:
HOME=$iso XDG_DATA_HOME=$iso/.local/share XDG_STATE_HOME=$iso/.local/state test/desktop-clean-launch.sh
```

两条路各起一次:`env -i` 的干净环境直接跑 `~/.local/bin/osmosis-desktop`,以及以用户
会话管理器的环境(`systemd-run --user`)跑 `gtk-launch io.github.osmosis`(菜单那条路)。
判据是 niri 报出 Osmosis 窗口、那个进程的 `/proc/<pid>/maps` 里有 libvulkan,不看观感。
要一个在跑的 niri 会话和显卡,所以不进 `just ci`;装过的二进制要先编好,编译照规矩去编译机。

## desktop-close-exit.sh —— 关窗后进程走不走(#15、#132)

```sh
just desktop-exit-check          # debug 构建,焦点与非焦点工作区各关一次
```

起实例、用 niri 的 `close-window` 关掉,断言 5 秒内退出且退出码 0。`unfocused` 先把窗口
挪到空工作区、焦点留在原地再关。直接调脚本时第一个参数是二进制,库路径由 `slint.nix`
那个 shell 给。

**会话锁着时判不了**:niri 锁屏时丢掉 IPC 来的几乎所有 action,`niri msg` 照样退 0,
close-window 石沉大海,进程当然不走 —— #132 就是把这个当成了 bug(alacritty 在同一台
锁着的机器上一样关不掉)。脚本关窗前先拿 `focus-window` 探一下,焦点没过来就退 2,
不给结论。编译机上用户不在,屏幕多半锁着,这时在它上面起一个嵌套 niri 再跑,不必去解
用户的锁:

```sh
niri -c /dev/null &    # 锁着的会话里照样起得来,它的日志报出新的 Wayland 与 IPC socket
NIRI_SOCKET=$XDG_RUNTIME_DIR/niri.wayland-2.<pid>.sock WAYLAND_DISPLAY=wayland-2 just desktop-exit-check
```

要合成器与显卡,不进 `just ci`。

## rollout.sh —— 发版推送脚本还守得住吗

```sh
test/rollout.sh
```

`release/rollout.sh`(`just rollout`,#128)要连 gh、pc3、真机和 nixos_config,不能拿真的
去试。这份把假的 `gh` / `ssh` / `scp` / `adb` / `nix` / `docker` / `nmap` 顶在 PATH 前面,
nixos_config 换成临时 git 仓库加本地裸远端,断言:签名不对不上传不装机、名单外设备不装、
离线设备进汇总、设备上读回的 APK 哈希要对、nixos_config 不干净或 prefetch 对不上就不动、
镜像要含 arm64。几秒跑完,改了 `release/` 就跑一遍。

## pick-e2e.sh —— 点一首歌,真的起播了、只发布了一次吗

跑之前:应用起着(`just desktop-dev` 或 `just mcp-android`)、已登录
(`test/mcp-login.sh`),`just server-dev` 与 `osmosis-pg` 在跑,每日推荐有歌。

```sh
test/pick-e2e.sh list       # 列表那条路
test/pick-e2e.sh wall       # 卡墙那条路(要 GPU 构建)
```

两条用户路径各跑一遍,每条都把同一首**连点三下**。判据两样,都查库:`play_events`
恰好多一行(有设备真的起播了),各设备队列的 `revision` 之和恰好涨 1(三下只发布了
一次)。输出在本机还是遥控别的设备都适用。

卡墙的卡画在 3D 纹理里,按坐标点第二下未必还命中同一张;脚本走场区上的无障碍动作
(`Increment` 挪选中、`Default_` 播选中的那张),读屏用户走的也是这条。

「起播 +1、发布 +0」先看应用日志里是不是 `队列数到上限了`:每台新设备(包括每个隔离的
测试实例)占账号一个队列槽,满 10 个就发布不上去。脚本判失败是对的,那是真问题。
窗口在锁屏或屏外时合成器一秒只给一帧,动画与相机慢到几十秒,脚本的等待按这个放宽过。

## pick-bench.py —— 点一首歌到出声花了多久(#137)

前提同 `pick-e2e.sh`。卡顿读数要 `OSMOSIS_STALL`(桌面运行期带上,APK 构建期带上)。

```sh
osmosis-desktop 2>&1 | test/pick-bench.py stamp > app.log     # 桌面日志每行加 epoch 秒
adb logcat -v epoch -s osmosis > app.log                       # 安卓自带
test/pick-bench.py drive --mode list --picks 0,1,2,3,0,1 --out list.jsonl
test/pick-bench.py drive --section 1 --playlist 0 --out bigq.jsonl   # 大队列:「我喜欢的」
test/pick-bench.py drive --output device --out remote.jsonl          # 遥控名字带 device 的那台
test/pick-bench.py report --picks list.jsonl --log app.log [--clock-offset 秒]
```

`drive` 每点一下等 `play_events` 多一行再静置几秒,记下点击时刻;`report` 按点击时刻
去日志里找这一下的 `act#… play` 各段与 `ui: 主线程卡了` 行,打成一行一下的表。遥控时
点的是控制端,日志读出声的那一端;跨机器时 `--clock-offset` 是日志时钟减本机时钟。
下标重复的那几下是「缓存命中」:同一首第二次点。

三个会让数字作废的现场条件,都踩过:

- **窗口拿不到帧**。锁屏会话里的嵌套 niri 一秒一帧,`drawn` 与卡顿全被帧间隔吞掉,卡墙
  动画走不完。编译机上用 `Xvfb :7` 加 `DISPLAY=:7`(去掉 `WAYLAND_DISPLAY`)能跑到
  三四十帧,核对日志里 `近 120 帧` 那行的 fps 再采。
- **队列槽满了**。`队列数到上限了` 时发布当场 409,大队列那组等于没发布,数字偏低。
- **被控端还锁着**。遥控器选回本机不通知被控端,那台仍挂着「正被 xx 遥控」,它自己的
  点歌全被锁挡掉(`played` 为 null)。先在它上面按「退出被遥控」。

## move-e2e.py —— 选设备时控制条还在、接着放了吗(#137 ③)

```sh
test/move-e2e.py --port 8091 --to 小米 --out move.jsonl          # 遥控器是桌面
test/move-e2e.py --ns-pid $(cat d1/ns.pid) --to 本机 --out back.jsonl  # ns-desktop 起的实例
```

选设备是一次迁移(目标准备 → 源停 → 目标从源停下的位置开始 → 确认)。脚本在遥控器上
按抽屉里的输出芯片,之后每 100ms 采一次:控制条(`PlayerBar::title`)在不在、曲名变没变、
状态行(`MainWindow::move-label`)走到哪。判据:控制条全程在、曲名全程是选之前那一首、
状态行出现过又消失(确认了)、确认之后进度读数不早于选之前 —— 是接着放,不是从 0:00 起。
每次采样写一行 JSON 到 `--out`。

它只看遥控器这一侧。源真的停了、目标真的响了,要另取两端的实际输出证据(桌面 PipeWire
流的 corked、安卓 `dumpsys audio` 的 AudioTrack 状态),以及两端日志里同一个操作号的
`迁移回话`(带位置)。

## playlist-fill-e2e.sh —— 歌单页铺满了吗

跑之前:应用起着、已登录(`test/mcp-login.sh`),账号里至少有一个歌单。

```sh
test/playlist-fill-e2e.sh   # 安卓默认 8090;桌面用 PORT=8091
```

进「我的歌单」,量歌单列表与第一个歌单详情的曲目列表离页底还空多少:控制条不在时
不超过页边距加一格间距(28px),在时恰好是 `BarMetrics.page-reserve`(106px,列表层
多一格间距)。#116 的两处空白 —— 页尾写死的 100px 垫块、空状态与空列表各占一截 ——
都会让它失败。无头测试(`crates/ui/tests/playlists.rs`)钉的是同一组数,这份补的是
「真机上铺出来也是这个数」。

真机上(`PORT=8090`)还多一道像素判据:元素树里有行不等于屏幕上画出来了。它用
`adb screencap` 截下列表自己那一块,灰度标准差 ≥ 0.06 才算画出了内容 —— 只剩背景
渐变的空列表区实测 0.033,画出行的 0.067~0.070。所以要有 `adb` 和 `magick`。
这道判据只会误报失败、不会误报通过:系统弹窗(通知授权)或软键盘盖在列表上时 sd 会
掉到 0.05 上下,先把它们关掉再跑。清过应用数据、或刚用 MCP 往登录框里灌过值之后,
这两样都常见。

## link-loss-e2e.sh 与 signal-gate.py —— 断线、接管失败、反复重启(#118),遥控器消失(#111)

`signal-gate.py` 是一道只管 `/signal` 的闸:HTTP 照常放行,信令可以 `kill -USR1` 掐断
(只掐客户端那一半,服务端要等探活才发现,与移动网络掉线同形)、`kill -USR2` 恢复。
整条网断掉的话点歌要经服务端取直链,本机本来就放不了歌,测不出锁有没有撤 —— 所以
断的只是信令。每台设备走自己的一道闸:手机经 `adb reverse tcp:3000 tcp:<闸端口>`,
namespace 里的桌面实例把闸起在 ns 自己的 `127.0.0.1:3000`、上游指 `10.0.2.2`。

```sh
test/signal-gate.py 3131 3118            # 手机那道:3131 → 本机 3118 那份 server
CTL=ns:<目录> TGT=android:<闸 pid> SERVER_LOG=<server 日志> test/link-loss-e2e.sh link-loss
CTL=android:<闸 pid> TGT=ns:<目录> SERVER_LOG=... RESTART_TGT=<拉起命令> test/link-loss-e2e.sh claim-fail
CTL=... TGT=... SERVER_LOG=... RESTART_CTL=<命令> RESTART_TGT=<命令> test/link-loss-e2e.sh restarts
CTL=... TGT=... SERVER_LOG=... test/link-loss-e2e.sh vanish   # #111
CTL=... TGT=... SERVER_LOG=... test/link-loss-e2e.sh blip     # #111
```

三条场景各对一条验收:被控端信令断了横幅就撤、本机点歌落账,恢复后遥控端回本机、
被控端不再被锁;掐住遥控端的信令让它名册停住,再让目标下线、去接管它,恢复后接管
失败、遥控端回本机、本机点歌落账;两台一起反复重启,服务端一次限流都没有。真相源是
`play_events` 行数与服务端日志(`设备入册`、`限流挡下一条请求`,server 要带
`RUST_LOG=info,server=debug`),安卓另查 `dumpsys audio` 的 `state:started`。

#111 的两条对「遥控器消失而被控端连接完好」:`vanish` 让被控端本机放着歌、被接管,
然后杀掉遥控端(安卓 `force-stop`、ns 实例 `SIGKILL`),断言租约满之前横幅还在、满了
自己撤掉、服务端记了「遥控器下线满租约」、被控端一直在出声;`blip` 掐断遥控端信令
几秒再放开,断言服务端记了「遥控器租约内续上」,再过一个租约被控端仍被它遥控。
`LEASE` 缺省 30,要与服务端 `control::LEASE` 一致。ns 实例的「在出声」看播放器自己的
位置日志,所以实例要带 `RUST_LOG=info,ui=debug` 起;不看 pipewire —— 应用一直开着
输出流,不放歌时那条流也是 running。

起播点一下就够。别连点两下:2026-09-23 在 ns 桌面实例上,同一行连点两下什么都没放。
namespace 实例怎么起、`ns:` 目录里要有什么,见脚本头注释。

## wheel-scroll-android.sh —— 外接鼠标滚列表还崩不崩(#120)

跑之前:`just mcp-android` 装的 debug 包在跑、已登录,要测的列表已经在屏上、停在顶端。
多台设备在线时带 `ANDROID_SERIAL`。

```sh
test/wheel-scroll-android.sh                                  # 每日推荐、歌单详情
LIST_ID=MusicPage::playlist-list test/wheel-scroll-android.sh # 我的歌单
```

滚轮来自系统自带的 `hid` 工具注册的虚拟 USB 鼠标,走的是和真鼠标一样的
`ACTION_SCROLL`。`input mouse scroll` 在 Android 14 上不存在,报 Unknown command
退出码却是 0,别换回去。先正负交替滚 20 格,断言 PID 没变、没有新的 crash 记录、
该 PID 的 logcat 里没有 `RustPanic` —— 输入回调里的 panic 被 android-activity 接住,
进程不死、界面冻住,只看 PID 认不出来。再往下两格、往上两格,断言列表首行标题
先换人再换回。

## 浏览器侧的对照页

本目录下的 `*.html` 是**排查性能问题时用来划定责任范围的最小对照页**。不含 Slint、
不含 wasm、不含 bevy,纯 HTML + JS,能把「浏览器本身的行为」和「我们这套技术栈的行为」
分开。

`just web-dev` 会把本目录的 `*.html` 一并复制进 `dist/web/`,所以起了 web 就能直接访问,
例如 <http://127.0.0.1:8073/rafprobe.html>。

曾经还有一个 `e2e/`(bun + Playwright),把这类对照实验自动化了,外加 24 个查 wasm 帧率
的一次性探针。帧率那轮排查结案(结论进了 `docs/wasm/`,补丁进了 slint fork)之后它再没人
跑过,2026-07-29 整个删掉。要翻当时的探针去 git 历史。

## rafprobe.html —— rAF 速率:空转 vs 持续呈现 WebGPU 画布

回答一个问题:**在这台机器的这个浏览器里,持续往 WebGPU 画布上呈现,会不会把页面的
`requestAnimationFrame` 拖慢?**

页面用和 3D 面板同量级的画布(1485×984),每帧只做一次 clear 并提交 —— 这是能构造的
最轻的呈现。两个按钮分别测空转和呈现中的 rAF 速率。

2026-07-19 在 144Hz 屏上实测:

| 场景 | rAF |
| --- | --- |
| 空转(只跑 rAF,不画) | 145 Hz |
| 持续呈现 WebGPU 画布 | 142 Hz |

注意这张表量的是 **rAF 间隔**,而 rAF 频率不等于真正呈现出去的帧数(实测过应用页
rAF 106/s、呈现只有 53/s)。按 `Display::DrawAndSwap` 重做过一遍,结论不变。

**结论:持续呈现 WebGPU 画布不掉帧。**所以当我们自己的 3D 页只有 59Hz 时,原因不在
浏览器、不在 WebGPU、也不在画布尺寸,而在我们这条链路(Slint 的 femtovg-wgpu 呈现路径,
或每帧的 GPU 提交方式)里。

## hostshape.html —— 宿主页面的对照件

与 `apps/web/index.html` 的结构、CSS、canvas 元素完全相同,只把 wasm 换成一条裸 WebGPU
循环。回答"帧率问题是宿主页面带来的还是 Slint 带来的"。实测 140fps,即宿主页面无辜。

留着它的理由:这类"是浏览器的锅还是我们的锅"的问题会反复出现,而每次重新搭一个对照页
都要花时间。同类问题再来时,照着它加一个按钮即可。
