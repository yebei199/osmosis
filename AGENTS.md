# AGENTS.md

给在本仓库里干活的 AI 助手。人看的文档在 [`README.md`](README.md),术语在
[`CONTEXT.md`](CONTEXT.md),架构决策在 [`docs/adr/`](docs/adr/) —— 这里只记**别人踩过、
不写下来就会再踩一遍**的操作陷阱。

## 验证 UI 改动:只用 `just shot`

```sh
just shot        # 宽版式(左侧导航栏)
just shot 420    # 紧凑版式(底部导航栏)—— 逻辑像素宽度,< 600px 即触发
```

产物在 `dist/shot.png`,是**合成器抓的真实窗口像素**。不要手搓这套流程,三个坑全在里面:

**1. `pkill -f app-desktop` 会杀掉你自己的 shell。**
`-f` 匹配整条命令行,而**你自己那条命令行里就有 "app-desktop" 这几个字**
(`pkill -f app-desktop && cargo build -p app-desktop`)。于是 pkill 把自己的父 shell 也算作
命中,整条命令带着退出码 144 消失。用 `pkill -x slint-study-desktop`(按进程名精确匹配),或者直接
`just desktop-kill`。

**2. 进程名是 `slint-study-desktop`,不是包名 `app-desktop`。**
`apps/desktop/Cargo.toml` 里 `[package] name = "app-desktop"` 但 `[[bin]] name =
"slint-study-desktop"`。拿包名去 `pkill` / 去跑 `target/debug/app-desktop`,**都不报错**
—— 只是没杀掉、没启动,而你还在对着十几分钟前的**老进程**截图,以为自己的改动没生效。

**3. 旧实例不杀干净,你截到的是上一版界面。**
同时跑两个 app,就有两个 "Slint Study" 窗口;更阴的是 MCP server 绑不上 8090 时**只在日志里
留一行 `Address already in use` 就继续跑**,AI 客户端按 `.mcp.json` 连过去,连上的是**旧进程**。
元素树、截图全是旧的,一切看起来"改了没生效"。

**4. MCP 的 `take_screenshot` 在 3D 页恒为纯黑。**
它走 Slint 的软件渲染器,**采不到 bevy 那张 wgpu 纹理**。3D 画面在它眼里永远是黑的,据此
断言"3D 没渲染出来"是错的 —— 那是工具的盲区,不是 bug。要看 3D 的真实像素,只能让合成器
抓窗口(`just shot` 干的就是这个)。

MCP 依然有用 —— 读元素树、量元素的真实尺寸/位置、模拟点击 —— 只是**别拿它的截图判断
渲染结果**。量尺寸尤其值得用:`get_element_properties` 会告诉你 LineEdit 其实有 56px 高,
而不是你以为的 32px。

## 判断 3D 页的遮挡有没有生效:量卡片边框,别看观感

那张注释卡片被物体挡住时,肉眼很难和「玻璃透出那个物体」区分开 —— 两者都是「卡片区域里
有一块蓝」。判据是**卡片那 1px 边框在不在**:

```sh
magick dist/shot.png -format "%[pixel:p{851,1055}]" info:   # 亮线 → 卡片在上
magick dist/shot.png -format "%[pixel:p{851,1070}]" info:   # 与背景同色 → 物体在上,遮挡生效
```

(坐标随窗口尺寸变,先用红色清除色把卡片矩形量出来再取点。)

按观感判过一次,判反了,又倒回去查了三轮管线 —— 相机、alpha 合成、深度清除值 —— 才发现
管线从第一帧起就是对的,近处物体只是没落进卡片矩形。**看不出效果时先确认取景,再怀疑管线。**

## `.slint` 热重载:各构建都能用,但验它有三个坑

结论先说:热重载在**所有构建下都正常**,包括 `bevy-3d`。bevy 只是把自己的 wgpu device 和纹理
交给 Slint,不干扰 live-preview。实测:

| 构建 | 日志有 `Reloaded component` | 画面上屏 |
| --- | --- | --- |
| 纯 Slint(skia) | 有 | 是 |
| `slint/unstable-wgpu-29`,不带 bevy、不驱动帧 | 有 | 是 |
| `bevy-3d`(wgpu + 每帧驱动 + 导航玻璃在跑) | 有 | **是**(探针换成按钮底色之后) |

写下这一节不是为了这个结论,是为了下面三个坑 —— 它们让人**连着四轮**得出「bevy-3d 下热重载
不工作」的错误结论,还差点把它写死进文档。

**1. `SLINT_LIVE_PREVIEW` 是构建期变量,不是运行期开关。**
`internal/compiler/generator/rust.rs` 在 slint 编译器里读它,决定生成哪套代码。只在跑二进制时
带上 → 产物里压根没有热重载机制 → 界面照常启动、改 `.slint` 毫无反应、**不报任何错**。
justfile 的 `desktop-dev` 写成 `SLINT_LIVE_PREVIEW=1 cargo run ...` 是对的:一条命令同时覆盖
构建和运行。分开 build 再 run 就两步都得带。nix-shell 会正常透传它。

**2. 用 `sed -i` 改 `.slint`,文件监听器收不到。**
`sed -i` 是「写临时文件 + rename」,inode 变了。必须**原地截断写**:
`sed ... "$bak" > /tmp/new && cat /tmp/new > 目标文件`,再用 `stat -c %i` 前后对比确认 inode 没变。

**3. GPU 构建里,你以为在看的那块颜色可能被一张导入的纹理盖住了。**
这个坑最贵。拿侧栏底色 `#0b0d13` 当探针,在非 GPU 构建下红得好好的,在 `bevy-3d` 下怎么改都不动
—— 因为侧栏里那句 `Image { source: root.nav-bg; width: 100%; height: 100% }` 把整条侧栏盖住了,
而 `nav-bg` 只在 GPU 构建里才有真纹理(其余构建是空图,宽度 0,盖不住任何东西)。于是**探针本身
把两组构建区分开了**,看起来像是热重载在 GPU 构建下失效。选探针前先在 `app.slint` 里确认它头顶
没有别的东西:内容区的按钮底色(`HoverButton` 的 `#4263eb`)是安全选择,侧栏和 3D 页都不是。

唯一可靠的判据是应用日志里的 `Reloaded component MainWindow from <绝对路径>`。光看画面变没变会
被两头骗:惰性渲染下没有输入就不重绘,而画面变了也可能是别的东西在动。

## 后台构建:同一时刻只留一个,并且验证产物真的更新了

wasm 构建要 5~8 分钟,期间很容易再起一个。**起新的之前先把旧的停掉**,并确认没有残留进程:

```sh
ps aux | grep -E "[c]argo|[n]ix-shell|[w]asm-bindgen"
```

不这么做时,失败是**静默的**,四种都真实发生过:

**1. 两个构建抢同一个 `target/`。**后完成的那个会用它自己的产物覆盖 `dist/`。如果它编的是旧
代码,你就在旧产物上验证新改动,而时间戳、日志全都看不出异常。

**2. 用 `;` 或 `&&` 把构建和 `just web-dev` 串成一个后台任务,停任务时会连坐。**
停掉"只是在读日志"的那个任务,SIGTERM 把整条 recipe 一起带走,日志里只留一行
`recipe web-dev was terminated by signal 15`。更阴的是旧 server 还在往同一个日志文件写访问
记录,日志看着一切正常。**构建和分发要拆成两个独立任务。**

**3. shell 的工作目录会留在上一条命令的位置。**一条命令里 `cd test/e2e` 之后,下一条命令
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

`just mcp-desktop*` 前置了端口守卫,占用时直接失败并点名占用者。最常见的占用者是上次
`just mcp-android` 留下的 `adb forward` —— 撤掉它:

```sh
adb forward --remove tcp:8090
```

## 提交前

`just ci` 逐字复述 `.github/workflows/ci.yml`。`dev` 分支的 push 不触发 CI,这是唯一的防线。
