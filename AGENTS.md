---
docs_synced_at: 188534f
---

# AGENTS.md

给在本仓库里干活的 AI 助手。

## 这个项目在干什么

把「Slint UI + Bevy 3D + 同一个 wgpu device」这条多端融合架构立住:一个进程、
一块显存、一条类型系统,一份代码出 desktop / android / web。音乐应用是首个载体,
用真实产品的压力检验它。完整叙述在 [`README.md`](README.md) 开头,能力上限的
推演在 [`docs/note/vision.md`](docs/note/vision.md)。

## 五问速查

| 问 | 答 |
|---|---|
| 这是什么 | 见上节;完整版在 [`README.md`](README.md) |
| 结构在哪 | [`README.md`](README.md)「目录结构」:cargo workspace,依赖方向严格单向 |
| 什么不能碰 | 依赖方向反向永久禁止;`[patch.crates-io]` 只写远程地址;`ANDROID_DEVICE_PIN` 不进任何进版本库的文件;UI 硬规则在 [`docs/design.md`](docs/design.md) |
| 待办在哪 | [`docs/TODO.md`](docs/TODO.md) |
| 怎么验 | `just ci` 逐字复述 CI,`dev` 分支的 push 不触发 CI,它是唯一防线;UI 改动用 `just shot` 与 MCP,贴屏幕边/随窗口变形的几何还要真机复核(`just mcp-android`),见下文 |

术语在 [`CONTEXT.md`](CONTEXT.md),架构决策在 [`docs/adr/`](docs/adr/)。
以下只记**别人踩过、不写下来就会再踩一遍**的操作陷阱。

## 两条总则:调试走 MCP,像素走 niri

**调试与交互一律走 MCP。** `just desktop-dev` 已经把 MCP 焊在里面
(feature `mcp` + 构建期 `SLINT_EMIT_DEBUG_INFO` + 运行期 `SLINT_MCP_PORT`,缺一不可),
不必记参数、也不必另起一条配方。要验证一个改动有没有生效,先想的应该是「读元素树 /
模拟点击 / 量尺寸」,而不是「截张图看看」。它能:

- `get_element_tree`、`query_element_descendants` —— 界面里到底有什么,而不是你以为有什么
- `click_element`、`set_element_value`、`dispatch_key_event` —— 真的走一遍用户路径
- `get_element_properties` —— 元素的真实尺寸与位置(LineEdit 实际 56px 高,不是你以为的 32px)

会话启动时 app 没跑的话,`.mcp.json` 里的 `slint-app` 不会连上,工具列表里也就没有它。
**别因此放弃**——那是个普通的 HTTP 端点,直接打 JSON-RPC 即可:

```sh
curl -s -X POST http://127.0.0.1:8091/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'
```

参数名用 camelCase(`elementHandle`、`matchElementTypeName`),写错会原地告诉你有哪些字段。

**逐像素为准的那张图靠 niri 抓窗口**,即 `just shot`。MCP 的截图现在也能看见 GPU 画面
(见下一节第 4 条),判断「画出来了没有」够用;比颜色、比合成结果时才需要合成器那张。

## 验证 UI 改动:`just shot` 与 MCP

```sh
just shot        # 宽版式(左侧导航栏)
just shot 420    # 紧凑版式(底部导航栏)—— 逻辑像素宽度,< 600px 即触发
```

产物在 `dist/shot.png`,是**合成器抓的真实窗口像素**。不要手搓这套流程,三个坑全在里面:

**1. `pkill -f app-desktop` 会杀掉你自己的 shell。**
`-f` 匹配整条命令行,而**你自己那条命令行里就有 "app-desktop" 这几个字**
(`pkill -f app-desktop && cargo build -p app-desktop`)。于是 pkill 把自己的父 shell 也算作
命中,整条命令带着退出码 144 消失。用 `pkill -x osmosis-desktop`(按进程名精确匹配),或者直接
`just desktop-kill`。

**2. 进程名是 `osmosis-desktop`,不是包名 `app-desktop`。**
`apps/desktop/Cargo.toml` 里 `[package] name = "app-desktop"` 但 `[[bin]] name =
"osmosis-desktop"`。拿包名去 `pkill` / 去跑 `target/debug/app-desktop`,**都不报错**
—— 只是没杀掉、没启动,而你还在对着十几分钟前的**老进程**截图,以为自己的改动没生效。

**3. 旧实例不杀干净,你截到的是上一版界面。**
同时跑两个 app,就有两个 "Osmosis" 窗口;更阴的是 MCP server 绑不上端口时**只在日志里
留一行 `Address already in use` 就继续跑**,AI 客户端按 `.mcp.json` 连过去,连上的是**旧进程**。
元素树、截图全是旧的,一切看起来"改了没生效"。

**4. GPU 构建里,你看的那一层可能不是真的。**

*你看不见自己改的东西*:`.slint` 元素会被导入的纹理盖住。侧栏那句
`Image { source: root.nav-bg; width: 100%; height: 100% }` 遮住了 `background: #0b0d13`,而
`nav-bg` **只有 GPU 构建才有真纹理**(其余是空图,宽度 0,谁也盖不住)。拿侧栏底色当探针,就会
得到"非 GPU 构建生效、GPU 构建不生效" —— **区分两组的是探针,不是被测的功能**。据此查过一个
不存在的 bug,四轮。选探针前先确认它头顶没东西:内容区实底元素(如 accent 绿
`#8fc46a` 的按钮)安全;侧栏不安全,Home 空槽与空状态按钮自光带按钮落地后也被
导入纹理盖着,同样不能当探针。

**MCP 的 `take_screenshot` 现在采得到导入的 wgpu 纹理**(2026-08-13 实测:桌面的卡墙封面与
光带按钮、真机的播放页点云与 fluid 条底都出得来)。这条曾经是反的 —— 旧文档说它恒为纯黑 ——
按那句话去断言"3D 没渲染出来"现在会得出错误结论。判断「画出来了没有」用它就够。

像素级的比对仍以 `just shot` 为准:那是合成器抓的真实窗口,MCP 出的是 Slint 自己渲的一张图,
两者的颜色与合成不保证逐像素相同。量尺寸也归 MCP:`get_element_properties` 会告诉你 LineEdit
其实有 56px 高,而不是你以为的 32px。

## 端到端验证走 MCP,断言写进脚本

跨了界面、api、server、数据库的链路(起播上报、登录、点播),单元测试够不着,**但也不该
交给人去点**。用应用内嵌的 MCP 驱动界面,拿一个真相源当断言,整条写成能重跑的脚本:

- 驱动:`find_elements_by_id` / `query_element_descendants` 找控件再 `click_element`,
  **按元素 id 找,不量坐标** —— 坐标随版式与窗口尺寸变,量出来的脚本活不过一次改版;
- 断言:数据库的行数、`get_element_properties` 量出来的尺寸、`assert` 得到的状态,
  不是截图,也不是"我看着对";
- 客户端没连上 app(会话启动时 app 没跑)就直接打 JSON-RPC,见上文那段 curl。

现成的一份:[`test/played-e2e.sh`](test/played-e2e.sh) —— 点一首歌,断言 `play_events`
多了一行。抄它的形状写新的。

## 判断遮挡有没有生效:量卡片边框,别看观感

封面卡被粒子挡住时,肉眼很难和「玻璃透出那颗粒子」区分开 —— 两者都是「卡片区域里
有一块亮色」。判据是**卡片那 1px 边框在不在**:

```sh
magick dist/shot.png -format "%[pixel:p{851,1055}]" info:   # 亮线 → 卡片在上
magick dist/shot.png -format "%[pixel:p{851,1070}]" info:   # 与背景同色 → 物体在上,遮挡生效
```

(坐标随窗口尺寸变,先用红色清除色把卡片矩形量出来再取点。)

按观感判过一次,判反了,又倒回去查了三轮管线 —— 相机、alpha 合成、深度清除值 —— 才发现
管线从第一帧起就是对的,近处物体只是没落进卡片矩形。**看不出效果时先确认取景,再怀疑管线。**

## `.slint` 热重载:`desktop-dev` 有

bevy 只是把自己的 wgpu device 和纹理交给 Slint,不干扰 live-preview。
得出过相反的结论,是探针选错(见上一节第 4 条),不是功能问题。

验它有两个静默陷阱,踩上就会误判成"功能不可用":

**1. `SLINT_LIVE_PREVIEW` 是构建期变量,不是运行期开关。**
`internal/compiler/generator/rust.rs` 在 slint 编译器里读它,决定生成哪套代码。只在跑二进制时
带上 → 产物里压根没有热重载机制 → 界面照常启动、改 `.slint` 毫无反应、**不报任何错**。
justfile 的 `desktop-dev` 写成 `SLINT_LIVE_PREVIEW=1 cargo run ...` 是对的:一条命令同时覆盖
构建和运行。分开 build 再 run 就两步都得带。nix-shell 会正常透传它。

**2. 用 `sed -i` 改 `.slint`,文件监听器收不到。**
`sed -i` 是「写临时文件 + rename」,inode 变了。必须**原地截断写**:
`sed ... "$bak" > /tmp/new && cat /tmp/new > 目标文件`,再用 `stat -c %i` 前后对比确认 inode 没变。

判据只认应用日志里的 `Reloaded component MainWindow from <绝对路径>`。光看画面会被两头骗:
惰性渲染下没有输入就不重绘,而画面变了也可能是别的东西在动。

## 后台任务:起新的之前先停旧的

**每次要起后台任务(`just desktop-dev*`、`just web-dev`、`cargo build`、`cargo run -p server`)
之前,先看一眼自己已有的后台任务,把同类的停掉再起**,否则一轮对话下来会攒出一串重复实例:
端口被占、`target/` 互相覆盖、截图截到老窗口,而这些失败全是静默的。

停任务只结束你启动的那条命令,不一定收走它的子进程 —— 再确认一次:

```sh
just desktop-kill                                       # 桌面实例(pkill -x osmosis-desktop)
ps aux | grep -E "[c]argo|[n]ix-shell|[w]asm-bindgen"   # 构建残留
```

`pkill -f app-desktop` 会连你自己的 shell 一起杀,原因见上文第 1 条。

## 后台构建:同一时刻只留一个,并且验证产物真的更新了

wasm 构建要 5~8 分钟,期间很容易再起一个。

不这么做时,失败是**静默的**,四种都真实发生过:

**1. 两个构建抢同一个 `target/`。**后完成的那个会用它自己的产物覆盖 `dist/`。如果它编的是旧
代码,你就在旧产物上验证新改动,而时间戳、日志全都看不出异常。

**2. 用 `;` 或 `&&` 把构建和 `just web-dev` 串成一个后台任务,停任务时会连坐。**
停掉"只是在读日志"的那个任务,SIGTERM 把整条 recipe 一起带走,日志里只留一行
`recipe web-dev was terminated by signal 15`。更阴的是旧 server 还在往同一个日志文件写访问
记录,日志看着一切正常。**构建和分发要拆成两个独立任务。**

**3. shell 的工作目录会留在上一条命令的位置。**一条命令里 `cd` 到子目录之后,下一条命令
里的 `git checkout crates/...`、`cp apps/web/index.html dist/web/` 全都找不到路径而失败 ——
而如果它们是用 `&&` 串起来的,**整条链会在这里停住,构建根本没跑**。产物于是停在上一版。
本次排查里发生过两次,其中一次量出来的数与上一轮几乎相同,看不出任何异常,差点当成
新结论。每条要改文件的命令都显式从仓库根开始。

**4. 测量之前先核对产物的时间戳。**

```sh
ls -la --time-style=+%H:%M:%S dist/web/app_web_bg.wasm
```

改了源码却量到旧产物,症状与"改动无效"完全一样,而后者会让你去推翻一个其实正确的假设。

## 后台任务:怎么知道它是死是活

Claude Code 的 Bash 工具跑在进程沙箱里,`ps` 与 `pgrep` **看不到**后台任务的进程。
实测过 `osmosis-desktop` 占着端口、而 `pgrep` 查无此进程。判死活只有三条路:任务完成
通知、落盘日志在长、产物时间戳变新。拿 `pgrep` 的空结果当"已经退了"会一路错下去。

**重构建命令不要接管道。**`cargo build | tail` 这类写法里,输出会被管道攒到进程结束才吐,
中途永远是空的,看起来和吊死一模一样。全量重定向进文件(`> log 2>&1`),再去读那个文件。

**首次进 nix 环境可能要下几百 MiB**(gradle 那套尤其),期间毫无输出。以为卡住而杀掉,
下次还得重下。

**5. `| tail -10` 会让你误判编译范围。**只保留 10 行的话,"某个 crate 有没有重编"根本看不到。
把完整日志重定向到文件,要看多少自己 grep。

**4. 结论会被上一层的瓶颈掩盖。**排查性能时尤其致命:Slint 在 wasm 上曾有个 16ms 定时器把
帧率压在 60,在它被拆掉之前,所有"减少工作量"的实验(降分辨率、关 MSAA、空转)都必然
"无效",而那些否定结论全是假的。**天花板存在时,不要从否定结果推出排除结论。**

分发 wasm 时,`just web-dev` 的 recipe 容易被信号打断,手动跑更稳:

```sh
nix-shell slint.nix --run 'wasm-bindgen target/wasm32-unknown-unknown/release/app_web.wasm \
    --target web --no-typescript --out-dir dist/web'
cp apps/web/index.html test/*.html dist/web/
```

**叫人去浏览器验证之前,必须确认 `dist/web/app_web_bg.wasm` 比
`target/wasm32-unknown-unknown/release/app_web.wasm` 新**:

```sh
[ dist/web/app_web_bg.wasm -nt target/wasm32-unknown-unknown/release/app_web.wasm ] \
    && echo 新 || echo 旧
```

跳过这一步,对方测的就是上一版产物 —— URL 参数被无视、开关"不生效",而你会去怀疑代码。

## 界面报「查询失败」时,先看 3000 上跑的是哪天的 server

`server` 不在 default-members 里,裸 `cargo build` 从不编它,于是本机 3000 上那个
进程可能是好几天前起的。新加的路由它没有,客户端拿到 404,界面只说一句「查询失败」——
看起来和「这个功能还没做」一模一样。个人主页的统计就这样被当成"后端聚合还没写"过一次,
而 `/stats` 早就在 `server/src/main.rs` 里了。

```sh
ss -ltnp "sport = :3000"            # 谁在跑
ps -o lstart= -p <pid>              # 什么时候起的
curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:3000/stats
```

## MCP 的两个端口:桌面 8091,真机 8090

两边可能同时开着,所以各占一个号(justfile 头部的 `desktop_mcp_port` 与 `mcp_port`),
`.mcp.json` 里也是两条:`slint-app` 指桌面,`slint-android` 指手机。

真机那个号**烧在 APK 里**(`apps/android/src/lib.rs` 的 `option_env!`),改它要重出包;
要挪就挪桌面那个。`just mcp-android` 建的 `adb forward tcp:8090` 留着不碍事,不必每次撤。

`just desktop-dev*` 仍前置端口守卫:桌面那个号被占时直接失败并点名占用者。分号之后
再撞上多半是上一个桌面实例没退干净,`just desktop-kill`。

## 装到真机上的是哪个档

开发装机走 `just mcp-android`,它把 native 库编成 **debug** 档。别拿
`just android-build`(release)的包去调界面:slint 的元素调试信息只在 debug 档
生成,release 包在手机上跑起来 MCP 只截得到图、`get_element_tree` 一个元素也查不到,
于是点按钮只能从全分辨率截图上量坐标,慢且容易点空。

代价是 APK 大得多(实测 debug 756MB 对 release 110MB),编译也更久。发布件仍走
`android-build`。

`target-android` 下的产物是按 profile 分开的两棵树,换档不会互相覆盖。仓库改过名的话
旧树里的 `CMakeCache.txt` 还记着老路径,`audiopus_sys` 会直接编不过 —— 删掉那棵树重来。

出包不必带 `ABIS`:默认只编 arm64-v8a。armeabi-v7a 有意不在默认里 —— skia-bindings
没有 armv7 的预编译产物,回退全量编 skia 还要 `ANDROID_NDK`,而 `Android.nix` 没导出它
(issue #47)。显式 `ABIS="armeabi-v7a"` 仍会撞上这一条,先解决 NDK 那步再说。

## 有一类界面 bug 桌面必然看不见

导航条、控制条这类**贴着屏幕边、或随窗口变形**的几何,桌面只能用来快速迭代,结论要在真机上
再看一遍。两个原因都不是偶然:

- **safe-area inset 桌面恒 0**。底栏高是 `64px + insets.bottom`,手机上多出来的那段是手势条。
  凡是拿「整条的高」当可用厚度算的几何,桌面上恰好等于 64、手机上多出四十几像素,画出来就
  压到手势条上。要传的是可用厚度,不要从目标纹理尺寸反推。
- **长宽比不同**。桌面窗口宽而扁,手机竖屏窄而高。同一个按归一化 uv 算的着色器(fluid 的
  羽流、navglass 的融合半径),两种比例下覆盖的区域完全不同:桌面上亮核落在时间读数那边,
  手机上正好压在随机键与循环键上,图标读不出来。

2026-08-12 的第六轮验收一次踩到三个,全是这两条的直接后果。

## 真机的锁屏密码

在 `.env` 的 `ANDROID_DEVICE_PIN`(该文件在 `.gitignore` 里,不进版本库)。装 APK、
重新授权 USB 调试、翻通知栏都要先解锁,而屏幕过一会儿就自己锁上。

```sh
set -a; source .env; set +a
adb shell input keyevent KEYCODE_WAKEUP
adb shell input text "$ANDROID_DEVICE_PIN"
adb shell input keyevent KEYCODE_ENTER
```

**不要把这个值写进任何进版本库的文件**,包括提交信息、issue 与注释。要引用它就引用
这个变量名。

## MIUI 装不上

`adb install` 报 `INSTALL_FAILED_USER_RESTRICTED: Install canceled by user`,是「开发者
选项 → USB 安装」这个开关没生效 —— 它联网校验之后会自己悄悄回退。先去手机上把它
关掉再打开(可能要重新验证小米账号),仍不行就绕:

```sh
adb push dist/osmosis-debug.apk /data/local/tmp/x.apk
adb shell pm install -i com.android.vending -r /data/local/tmp/x.apk
adb shell rm /data/local/tmp/x.apk
```

## `[patch.crates-io]` 只写远程地址

`Cargo.toml` 的 patch 永远指向 `git = "https://github.com/yebei199/..."`,不写
`path = "../slint-fork/..."` —— 哪怕只是临时验证一下再改回来。本机路径进了仓库,CI 和
docker 就拉不到,而本地 `cargo check` 照样通过,谁都发现不了。

同步 fork 时因此是**先推 fork,再验本仓库**:fork 的 `dev` 推上去,本仓库
`cargo update -p slint -p slint-build` 跟进锁文件,然后才跑验证。推之前先建当日的
`backup/dev-<日期>`,验证不过就从它回滚重来。

`dev` 靠 **merge 上游**跟进,不 rebase,因此推送永远是快进 —— 推之前 `--dry-run`
一次,需要 `--force` 就说明中途做了 rebase 或 amend,停下来查,别顺手加 force。
代价是历史里会留下被上游顶掉的死 commit,所以补丁清单以 `Cargo.toml` 里
`[patch.crates-io]` 上方那段注释为准,不看 `git log`。步骤见 `docs/note/fork-sync.md`。

## 提交前

`just ci` 逐字复述 `.github/workflows/ci.yml`。`dev` 分支的 push 不触发 CI,这是唯一的防线。
