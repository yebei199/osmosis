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
test/pick-e2e.sh search     # 搜索那条路:搜 KEYWORD(缺省「晴天」),点一行结果
test/pick-e2e.sh queue      # 队列页那条路:展开播放页、打开队列,点一行(要已经在放)
```

两条用户路径各跑一遍,每条都把同一首**连点三下**。判据两样,都查库:`play_events`
恰好多一行(有设备真的起播了);各设备队列的 `revision` 之和涨 1(新的一批只发布了一次),
或者不涨但 `play_queue_reports` 记了一个新检查点(这一批早已同步上去,#137 ③ 之后
同一批不再重复发布)。两次以上就是连点又在重复发布。

**组内**(#142):设 `OTHER_PORT` 为组里另一台的 MCP 端口,两台事先已在同一个组里
(输出设备那一排按「+」)。组里点歌只改服务端的全局状态,判据换成:`play_events` 恰好
+1(服务端记的)、`play_groups.version` 恰好 +1(连点三下只发一次意图),并且 30 秒内
**两台**控制条上的曲名(`PlayerBar::title`)都换成库里全局状态那一条的曲名。四条入口
在两台上各跑一遍,就是「组里任何一台从任何入口点歌,两台都换过去」:

```sh
OTHER_PORT=8090 test/pick-e2e.sh list     # 在桌面上点,手机跟着换
PORT=8090 OTHER_PORT=8091 test/pick-e2e.sh queue   # 在手机的队列页点,桌面跟着换
```

卡墙的卡画在 3D 纹理里,按坐标点第二下未必还命中同一张;脚本走场区上的无障碍动作
(`Increment` 挪选中、`Default_` 播选中的那张),读屏用户走的也是这条。

**同一账号下只能有被测的那一台在放。** `play_events` 只记账号不记设备,脚本等起播的
那一分钟里,另一台实例的自动续播(或者被测这台自己那首刚好放完)也会让它 +1,于是
「起播 +1、发布 +0、检查点无」其实是点击被丢了、账算到了别处。2026-09-24 手机卡墙那条
就这样误判过两次:一次是开着的桌面实例在续播,一次是点中在放的那首后原曲放完续播了。
判不准时对一眼被测那台的日志里有没有新的 `act#… play begin`。

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
test/pick-bench.py drive --output device --out remote.jsonl          # 组里让名字带 device 的那台出声
test/pick-bench.py report --picks list.jsonl --log app.log [--clock-offset 秒]
```

`drive` 每点一下等 `play_events` 多一行再静置几秒,记下点击时刻;`report` 按点击时刻
去日志里找这一下的 `act#… play` 各段与 `ui: 主线程卡了` 行,打成一行一下的表。在组里时
点的是控制端,日志读出声的那一端;跨机器时 `--clock-offset` 是日志时钟减本机时钟。
下标重复的那几下是「缓存命中」:同一首第二次点。

三个会让数字作废的现场条件,都踩过:

- **窗口拿不到帧**。锁屏会话里的嵌套 niri 一秒一帧,`drawn` 与卡顿全被帧间隔吞掉,卡墙
  动画走不完。编译机上用 `Xvfb :7` 加 `DISPLAY=:7`(去掉 `WAYLAND_DISPLAY`)能跑到
  三四十帧,核对日志里 `近 120 帧` 那行的 fps 再采。
- **队列槽满了**。`队列数到上限了` 时发布当场 409,大队列那组等于没发布,数字偏低。
- **本机还在组里**。在组里点歌改的是组的全局状态,出声的是组里的出声设备;要量本机
  自己的起播,先按横幅上的「退出」回到独奏(#142)。

## views-e2e.py —— 进「我喜欢的」时别的视图的歌冒不冒出来(#137 ④)

```sh
test/views-e2e.py --port 8091 --out views.jsonl                 # 桌面
test/views-e2e.py --port 8090 --out views.jsonl --steps enter,aba   # 小米
```

三步:先记下每日推荐那一批再进我的歌单 → 我喜欢的(`enter`);我喜欢的 → 另一个歌单 →
我喜欢的、中间不等加载(`aba`);设置页退出、`mcp-login.sh` 登回来再进(`relogin`)。
每一步最后一次点击之后连续采样列表前几行的无障碍标签(曲目行的标签就是歌名),判据是
每次采样要么是加载态的空列表,要么每一首都属于收尾时那一批 —— 一次越界就失败。
不点任何一首歌,不出声。首启无缓存与有缓存各跑一遍:前者用全新的状态目录(安卓是
清掉应用数据),后者原样重启。

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

## link-loss-e2e.sh 与 signal-gate.py —— 掉线、服务端重启、反复重启(#142)

`signal-gate.py` 是一道只管 `/signal` 的闸:HTTP 照常放行,信令可以 `kill -USR1` 掐断
(只掐客户端那一半,服务端要等探活才发现,与移动网络掉线同形)、`kill -USR2` 恢复。
每台设备走自己的一道闸:手机经 `adb reverse tcp:3000 tcp:<闸端口>`,namespace 里的
桌面实例把闸起在 ns 自己的 `127.0.0.1:3000`、上游指 `10.0.2.2`。

```sh
test/signal-gate.py 3131 3118            # 手机那道:3131 → 本机 3118 那份 server
OUT=android:<闸 pid> PEER=ns:<目录> SERVER_LOG=<server 日志> test/link-loss-e2e.sh link-loss
OUT=... PEER=... SERVER_LOG=... test/link-loss-e2e.sh last-output
OUT=... PEER=... SERVER_LOG=... RESTART_SERVER=<命令> test/link-loss-e2e.sh server-restart
OUT=... PEER=... SERVER_LOG=... RESTART_OUT=<命令> RESTART_PEER=<命令> test/link-loss-e2e.sh restarts
```

四条场景对 #142 的掉线规则(`docs/adr/0032` 第五节):`link-loss` 让 OUT 与 PEER 一起
出声,掐断 OUT —— OUT 立刻停下、PEER 照放、库里组仍在放,恢复后 OUT 照最新状态接着出声;
`last-output` 只让 OUT 出声,掐断它 —— OUT 立刻停下,服务端探活发现后把组置为暂停(日志
「出声设备出册」),恢复后 OUT 照组状态停着、PEER 按 ⏯ 才一起接着放;`server-restart`
重启服务端 —— 组还在库里、版本不回退,两台重新入册,OUT 照状态接着出声;`restarts` 两台
一起反复重启,服务端一次限流都没有。

真相源是 `play_groups` 那一行(`playing`、`version`)与服务端日志(`设备入册`、
`出声设备出册`、`限流挡下一条请求`,server 要带 `RUST_LOG=info,server=debug`)。「在出声」
安卓看 `dumpsys audio` 里本应用的 `state:started`,ns 实例看播放器自己的位置日志,所以
实例要带 `RUST_LOG=info,ui=debug` 起;不看 pipewire —— 应用一直开着输出流,不放歌时那条流
也是 running。

这套推翻了 #118/#111 的旧验收(断线就撤被控锁、遥控器租约、接管失败回本机):遥控器与
控制权槽位整个删了(#142 的决定),`claim-fail`、`vanish`、`blip` 三条随之删除。
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
