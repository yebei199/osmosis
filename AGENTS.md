# AGENTS.md

给在本仓库里干活的 AI 助手。人看的文档在 [`README.md`](README.md),术语在
[`CONTEXT.md`](CONTEXT.md),架构决策在 [`docs/adr/`](docs/adr/) —— 这里只记**别人踩过、
不写下来就会再踩一遍**的操作陷阱。

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
curl -s -X POST http://127.0.0.1:8090/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'
```

参数名用 camelCase(`elementHandle`、`matchElementTypeName`),写错会原地告诉你有哪些字段。

**桌面端的真实像素只能靠 niri 抓窗口**,即 `just shot`。原因见下一节:MCP 的
`take_screenshot` 走软件渲染器,采不到 GPU 纹理。两个工具分工明确 —— MCP 回答「有什么、
是多大、点了会怎样」,niri 回答「画出来长什么样」。

## 验证 UI 改动:只用 `just shot`

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
同时跑两个 app,就有两个 "Osmosis" 窗口;更阴的是 MCP server 绑不上 8090 时**只在日志里
留一行 `Address already in use` 就继续跑**,AI 客户端按 `.mcp.json` 连过去,连上的是**旧进程**。
元素树、截图全是旧的,一切看起来"改了没生效"。

**4. GPU 构建里,你看的那一层可能不是真的。两个方向都会骗人。**

*工具看不见真东西*:MCP 的 `take_screenshot` 走 Slint 的软件渲染器,**采不到 bevy 那张 wgpu
纹理**,3D 画面在它眼里恒为纯黑。据此断言"3D 没渲染出来"是错的 —— 那是工具的盲区,不是 bug。

*你看不见自己改的东西*:反过来,`.slint` 元素会被导入的纹理盖住。侧栏那句
`Image { source: root.nav-bg; width: 100%; height: 100% }` 遮住了 `background: #0b0d13`,而
`nav-bg` **只有 GPU 构建才有真纹理**(其余是空图,宽度 0,谁也盖不住)。拿侧栏底色当探针,就会
得到"非 GPU 构建生效、GPU 构建不生效" —— **区分两组的是探针,不是被测的功能**。据此查过一个
不存在的 bug,四轮。选探针前先确认它头顶没东西:内容区按钮底色(`#4263eb`)安全,侧栏不安全。

真实像素只能让合成器抓窗口(`just shot` 干的就是这个)。MCP 依然有用 —— 读元素树、量元素的
真实尺寸/位置、模拟点击 —— 只是**别拿它的截图判断渲染结果**。量尺寸尤其值得用:
`get_element_properties` 会告诉你 LineEdit 其实有 56px 高,而不是你以为的 32px。

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

## 端口 8090 被占

`just desktop-dev*` 前置了端口守卫,占用时直接失败并点名占用者。最常见的占用者是上次
`just mcp-android` 留下的 `adb forward` —— 撤掉它:

```sh
adb forward --remove tcp:8090
```

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

## 提交前

`just ci` 逐字复述 `.github/workflows/ci.yml`。`dev` 分支的 push 不触发 CI,这是唯一的防线。
