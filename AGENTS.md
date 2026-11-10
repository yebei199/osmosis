# AGENTS.md

## 这是什么

Osmosis:一个带 3D 视觉的音乐播放器(搜歌、边下边播、多设备遥控、封面点云与卡墙),
建在「Slint UI + Bevy 3D + 同一个 wgpu device」上,一份 Rust 代码出 linux 桌面与 android,
外加一个 axum 后端。用户自己的项目,发行件装在用户自己的桌面、平板和手机上。产品叙述见
[`README.md`](README.md),术语 [`CONTEXT.md`](CONTEXT.md),架构决策 [`docs/adr/`](docs/adr/),
UI 硬规则 [`docs/design.md`](docs/design.md),待办 [`docs/TODO.md`](docs/TODO.md)。

## 结构

Cargo workspace,依赖方向严格单向(`cargo xtask boundaries` 检查,ADR 0001/0002):
`apps/* → ui → {app-core, api, syncplay} → contract`;`audio` 由 `ui` 注入;
`apps/{desktop,android} → render3d` 不经过 `ui`。

| 路径 | 内容 |
|---|---|
| `crates/contract` | 线上格式,只依赖 serde |
| `crates/app-core` | 客户端领域状态与规则;不依赖 slint、不做 IO、不认识 `api` |
| `crates/api` | HTTP 客户端;`Send` 约束关在这里 |
| `crates/audio`、`crates/syncplay` | 出声;设备间信令(在线名册、遥控) |
| `crates/render3d` | bevy 在共享 wgpu device 上离屏渲染成 `slint::Image` |
| `crates/ui` | `.slint` 界面(`slint/`)+ 组装点;`tests/` 是无头 UI 测试 |
| `apps/desktop` | 桌面入口,包名 `app-desktop`,二进制与进程名 `osmosis-desktop` |
| `apps/android` | android 入口(cdylib)+ `gradle/` |
| `apps/web`、`apps/ios` | 废弃,不是 workspace 成员;新代码不必兼容它们 |
| `server/` | axum 后端,把 bang-dream(独立仓库)的 gRPC 翻成 HTTP/JSON;`proto/` 是上游契约副本,`migrations/` 是 Postgres |
| `xtask/` | 构建逻辑(`cargo xtask android`、`boundaries`) |
| `test/` | 端到端脚本、`headless/`(无头 sway 夹具)、`mcp-e2e/`、`remote-behavior/` |
| `release/` | `just rollout`:把已发布版本推到各设备与 nixos_config |
| `docs/note/` | 操作手册与调查记录;日常命令和验证的坑在 [`dev-workflow.md`](docs/note/dev-workflow.md) |

## 会咬人的事实

- **门禁是 `just ci`**,逐字对应 `.github/workflows/ci.yml`(fmt、nextest+clippy `-D warnings`、
  android 交叉 clippy、boundaries)。CI 只在非 draft PR 和手动触发时跑,
  直接 push 任何分支(含 master)都没有 CI。`ci-test` 依赖 Docker 里的 Postgres(`just pg`)
  与 RustFS(`just rustfs`)。`test/remote-behavior/` 与 `test/mcp-e2e/`(`just mcp-e2e`)不在其中,
  按需手动跑。
- **agent 只验证编译和逻辑时用 `just agent-build`、`just agent-test`**:依赖不优化,冷构建快一倍多,
  但 bevy 运行很慢,3D 帧率和手感要用 dev 档验证。
- **裸 `cargo build/test` 只走 `default-members`**(桌面链路)。`audio`、`syncplay`、`server`、
  `xtask` 要 `-p` 点名才会测;`apps/android` 在宿主 target 上编不过,只经 `cargo xtask android`
  或 `--target aarch64-linux-android`。
- **原生依赖来自 `nix-shell slint.nix`(桌面)与 `Android.nix`(安卓)**;工具链钉在
  `rust-toolchain.toml`。android 产物在 `target-android/`,默认只编 arm64-v8a;armv7 缺 skia
  预编译且 `Android.nix` 不导出 NDK。不要设 `RUSTFLAGS` 环境变量:它进依赖指纹,会让每个入口各维护
  一棵依赖树,而且会盖掉 `.cargo/config.toml` 里的 `-Zthreads`。工具链是钉住的 nightly,新布局、细粒度锁和
  `-Zthreads` 都依赖它。x86_64 链接器由全局 `clang-mold` 提供,不要在仓库里覆盖。cargo 中间产物在全局 `build.build-dir`(`~/.cargo/config.toml`,仓库外)。
- **重构建命令不能前台跑**:`.claude/hooks/no-foreground-builds.sh` 会拒绝前台的 nix-shell、
  交叉编译和 `just ci*/shot/android-*`。
- **`[patch.crates-io]` 只写远程 git 地址**(slint fork 的 `dev` 分支)。本机 `path =` 本地照样
  通过,CI 和 docker 拉不到。fork 补丁清单以 `Cargo.toml` 里那段注释为准,同步见
  [`docs/note/fork-sync.md`](docs/note/fork-sync.md)。
- **后端地址烘在编译期**(`crates/api` 的 `option_env!("OSMOSIS_API_BASE")`)。debug 配方清掉它,
  连本机 `127.0.0.1:3000`(`just server-dev` + `just bang-dream`);release 配方烘 justfile 的
  `api_base`(生产)。两者不能一样。没烘地址的构建把登录态存在 `osmosis-dev/`,装机版在 `osmosis/`。
- **`server` 不在 default-members**,本机 3000 上可能跑着几天前的进程;新路由 404 时界面只说
  「查询失败」。
- **应用内嵌 MCP**:桌面 8091、真机 8090(真机端口经 `option_env!` 烧进 APK),`.mcp.json` 两条
  各指一台。只有 debug 档(`just desktop-dev`、`just mcp-android`)带元素调试信息;release 包里
  元素树是空的。
- **AI 会话里的 debug 桌面构建拒绝开到真实显示**(`apps/desktop/src/display_guard.rs` 认
  `CLAUDECODE`/`CODEX_*`);`just desktop-dev`、`just shot`、e2e 夹具跑在 `test/headless/` 的无头 sway
  里,声音进 `test/null-sink.ts` 的 null sink。真机扬声器不在这套机制里。
- **桌面上 safe-area inset 恒为 0、长宽比与手机相反**:贴屏幕边或随窗口变形的几何,桌面结论不
  代表真机。
- **发版**:版本在 `Cargo.toml` 的 `workspace.package.version`;推 `v*` tag 触发
  `release.yml`,只出桌面二进制与服务端镜像;APK、设备安装与 nixos_config 的 pin
  (`home/features/desktop/osmosis.nix`)归 `just rollout`。桌面端由 nixos_config 部署,不由本仓库安装。

## 边界

- 依赖方向不得反向。
- 只在主 checkout 上串行开发:不开 git worktree,也不让几个 agent 同时改代码。
  几条开发线会争用同一个 target 产物槽位，互相覆盖，测试结果因此可能假红，也可能假绿。
  只读调查可以并行。
- `.env`(gitignored)里的 `ANDROID_DEVICE_PIN`、`TEST_USERNAME`/`TEST_PASSWORD` 与邀请码只按变量名
  引用,值不进任何入库文件、提交信息、issue 或注释。
- 用户本人要看窗口或听声音时才用 `OSMOSIS_SHOW_WINDOW=1` / `OSMOSIS_REAL_AUDIO=1`。
