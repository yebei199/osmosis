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

**3. `| tail -10` 会让你误判编译范围。**只保留 10 行的话,"某个 crate 有没有重编"根本看不到。
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
