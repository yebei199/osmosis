# 手动同步 fork

本项目的 slint 走 `yebei199/slint` 的 `dev` 分支,它又把 femtovg 指到上游
`femtovg/femtovg` 的 git master(femtovg#302 已合并,fork 已退役;等上游发出含
#302 的版本后改回 crates.io)。fork 不自动跟进上游,需要时按本文跑一遍。

不上定时任务是刻意的:`master` 要保持从上游**纯快进**,而 GitHub 的定时 workflow
只从默认分支读取,往 master 塞一个上游没有的 workflow 文件就把快进本身破坏了。

## 一、fork 的分支各是什么

`yebei199/slint`:

| 分支 | 内容 | 维护方式 |
|---|---|---|
| `master` | 上游 master 的镜像,零本地改动 | 纯快进 |
| `dev` | 本地补丁,加上历次合进来的上游 master,本项目实际依赖 | merge 上游,快进推 |
| `fix/*` | 向上游提 PR 的分支,各自从上游 master 拉 | 只在 PR 需要时 rebase |
| `backup/dev-*` | 上一次同步前的 dev,出事能回滚 | 每次同步前更新 |

`dev` 从 rebase 改成 merge 是 2026-08-13 的决定,理由是**不再强推**。rebase 把补丁在新
base 上重放,产出新 sha,新 tip 不是旧 tip 的后代,git 于是只接受 `--force` ——「必须强推」
从来不是 git 或 cargo 的要求,是选了 rebase 的必然结果。merge 是追加,推送是快进。

代价要认:合并之后 `git log upstream/master..dev` 列出的不再等于「我们还背着的补丁」。
上游若自己修了同一个缺陷,冲突解决会把我们那条的效果抹掉,而那个 commit 仍挂在名单上
(2026-08-13 的 `be095f1e4` 就是这样一条死 commit)。所以**补丁清单的权威副本不在 git
历史里**,在 `Cargo.toml` 的 `[patch.crates-io]` 上方那段注释。核对补丁时看那里。

`yebei199/femtovg` 已于 2026-08-18 退役:它唯一承载的 femtovg#302 于 2026-08-17
并入上游 master,slint fork 从此直接指上游 git。两条分支同日删除,见下节。

角色分工:`master` 镜像上游,`dev` 承载全部本地改动,PR 从 `dev` 拆出去单独开分支,
上游收下多少就从 `dev` 撤掉多少。PR 一旦合并或关闭,对应分支就该删,内容自然留在上游
代码或 PR 正文里 —— femtovg fork 走完这条路后整个退役,就是这套规则的终点。

三条补丁分别是什么、各自的上游去向,写在 `Cargo.toml` 的 `[patch.crates-io]` 上方。
2026-08-18 按三点 diff 核对过,`dev` 相对上游独有的改动正好只有这三处代码加一个
`Cargo.lock`,与那份清单一致:

| 文件 | 补丁 | 撤销条件 |
|---|---|---|
| `internal/backends/winit/frame_throttle.rs` | wasm 上交给浏览器的 requestAnimationFrame 定帧 | 未提 PR,上游也没自己修 |
| `internal/core/api.rs` | `Window::is_active()` | 未提 PR,上游无等价物 |
| `internal/renderers/femtovg/Cargo.toml` | femtovg 走上游 git 而非 crates.io | 上游发出含 femtovg#302 的版本 |

## 已删除的分支

2026-08-18 清了两条(`yebei199/femtovg`,fork 退役):

| 分支 | 删时的 sha | 内容去了哪 |
|---|---|---|
| `dev` | `0a2767199` | femtovg#302 已合进上游 master。历史 Cargo.lock 按 `?branch=dev` 锁过这个 sha,所以删前打了 `archive/dev-2026-08-18` tag 指着它,commit 保持可达、可按 sha fetch |
| `pr/wgpu-per-draw-allocations` | — | #302 的 PR 分支。commit 永久挂在上游的 `refs/pull/302/head`,PR 页面可见,无需归档 |

2026-08-13 清了两条(`yebei199/slint`):

| 分支 | 删时的 sha | 内容去了哪 |
|---|---|---|
| `fix/wgpu29-present-order` | `ec8329e24` | slint#12861 已合,`88c9c7d` 是上游 master 的祖先,代码在 `internal/renderers/skia/wgpu_29_surface.rs`。分支上独有的只有一个刷新用的 merge commit |
| `dev-rebased-2026-08-13` | `d865051fa` | 走非强推路线时的中间产物。与 `dev` 的树逐字节相同(`git diff` 为空),没有独有内容 |

2026-07-29 两个 fork 各清了一批,内容都已另有归宿。

`yebei199/slint`:

| 分支 | 内容去了哪 |
|---|---|
| `fix/femtovg-wgpu-texture-import-wasm` | slint#12539 合进上游,代码已在上游 master 里 |
| `fix/femtovg-wgpu-svg-upscale-wasm` | slint#12541 已关。真正的修复是 femtovg#301,随 femtovg 0.26 进上游(slint#12553);诊断过程留在那两个 PR 的正文里 |
| `probe/wgpu-present-sync` | 结论与统计在 `../ready_issue/slint-wgpu-present-order.md`,补丁正文也内联在那份文档里 |

`yebei199/femtovg`:

| 分支 | 内容去了哪 | 核对方式 |
|---|---|---|
| `examples/many-rects` | femtovg#304 已合 | 上游有 `examples/many_rects.rs` |
| `feat/image-source-html-canvas` | femtovg#300 已合 | 上游 `src/image.rs` 有 `HtmlCanvasElement` |
| `fix/wgpu-html-image-natural-size` | femtovg#301 已合 | 上游 `src/renderer/wgpu.rs` 有 `rasterize_to_canvas` |
| `fix/wgpu-cache-static-bindings`、`pr/wgpu-cache-static-bindings` | femtovg#303 已关,内容并进 #302 | #302 分支里 sampler 缓存在位 |
| `perf/wgpu-resident-buffers` | 5 commit 的旧版,被 #302 的 10 commit 版覆盖 | pin 分支已改指新版 |

删分支前先确认它的内容在别处能找回。想删掉分支又保住 commit,标准做法是打 tag:
tag 一样让 commit 可达、可按 sha fetch,但不占分支列表,也不会被误当成还在维护的
分支。历史 Cargo.lock 用 `?branch=` 锁过的分支尤其要这样处理 —— 锁文件记的是 sha,
只要 sha 还可达,旧提交就还能构建。PR 分支不用打:commit 永久挂在上游的
`refs/pull/<n>/head`,PR 页面也一直在。

判断「上游是否已经收下」不能用 `git rev-list --count upstream/master..branch`。squash
合并会换掉哈希,分支看上去永远领先。要按内容查:去上游代码里找那个函数、那个文件、
那个符号在不在。

同理,查「fork 还背着什么」要用**三点** diff,不能用两点:

```bash
git -C ~/RustroverProjects/slint-fork fetch upstream
git diff upstream/master...origin/dev --stat   # 三个点
```

两点 diff 比的是两个 tip 的树,上游自己新增的 commit 会被算成「差异」,反着显示成我们
删了几千行。三点从 merge-base 起算,列出来的才是 fork 独有的改动。2026-08-18 实测:
两点报 237 个文件,三点只有 4 个 —— 后者才是真相。

探针那条差点漏掉。结论早就写下了,但复现步骤当时指向分支本身,那 11 行补丁只存在于
那条分支上。

## 二、同步步骤

### 1. master 快进

```bash
cd ~/RustroverProjects/slint-fork
git fetch upstream
git push origin upstream/master:refs/heads/master
```

`push-guard` 会拦主分支,这是设计如此。确认要更新后重跑同一条命令。

### 2. femtovg 跟进上游

fork 退役后没有重建步骤:femtovg 直接指上游 git master,跟进就是在 slint fork 里
`cargo update -p femtovg` 再走第 3、4 步。上游发出含 #302 的版本后,把
`internal/renderers/femtovg/Cargo.toml` 里的 git 依赖改回 crates.io,本文相关段落
一并删掉。

### 3. dev 合进上游

先备份,再把上游合进来:

```bash
cd ~/RustroverProjects/slint-fork
git branch -f backup/dev-$(date +%F) dev
git push origin backup/dev-$(date +%F)
git checkout dev
git merge upstream/master
```

冲突出现在哪,哪条补丁就该重新问一遍「上游是不是已经自己修了」。判断方法:去上游
master 里找对应的代码,看那个缺陷还在不在。上游修了就取上游那侧
(`git checkout --theirs <文件>`),我们那条从此是死 commit,把它从 `Cargo.toml`
的补丁清单注释里划掉 —— 那份注释才是权威副本,git 历史不是。

没冲突不等于没被上游收下。squash 合并会换掉哈希,内容一样也不会撞车。所以**每次
都要逐条按内容核对**,不能只看 merge 干不干净。

### 4. 先推,再验

顺序是反直觉的:先把 `dev` 推上去,再验证本项目。`Cargo.toml` 的 patch 不许改成
`path = "../slint-fork/..."`,哪怕只是临时的 —— 本机路径进了仓库,CI 和 docker 拉不到,
而本地验证照样通过,谁都发现不了。回滚成本由第 3 步的备份分支兜底。

```bash
cd ~/RustroverProjects/slint-fork
git push --dry-run origin dev   # 应当是 `旧..新` 两点,带 `+` 就说明历史被重写了
git push origin dev
```

推之前先 dry-run。走 merge 路线时它必须是快进;需要 `--force` 就说明中途做了 rebase
或 amend,停下来查清楚,不要顺手加 force。

回本项目跟进锁文件后再验:

```bash
cargo update -p slint -p slint-build
nix-shell slint.nix --run 'cargo check --workspace && cargo test -p ui -p app-desktop -p render3d -p app-core -p audio'
```

`cargo check --workspace --all-features` 不能用:那会把 `mcp` 也一起开进来,而它是个
调试后门(理由见 `apps/desktop/Cargo.toml` 的 feature 注释)。

`cargo update` 要对**每个**被 patch 的包点名。cargo 不会自动放弃锁里已有的 registry
版本,少写一个就静默变成 `patch.unused`,补丁不进产物还不报错。

验证报出来的错未必是 fork 的。2026-08-08 这次,`cargo check` 说 `api::settings::Settings`
没有 `dark` 字段,而源码里明明有 —— 是 `target/` 里那个 `api` 单元的 fingerprint 陈旧,
cargo 认为它是新的就没重编,`ui` 于是链到了加 `dark` 之前的元数据。换 pin 只是碰巧让
`ui` 重编、把这个早就存在的陈旧缓存暴露出来。判断方法:错误指向的字段在源码里存在,
就 `touch` 那个 crate 的源文件重跑一次,别去 fork 里找原因。

femtovg 的补丁只在 wasm 上有意义,而 `cargo clean -p femtovg` **只清宿主 target**。
不带 `--target` 地清一遍再 `cargo check --target wasm32-unknown-unknown`,日志里
femtovg 根本不出现 —— 那份 wasm 单元原封不动地复用了缓存,而命令返回 0。想真的重编:

```bash
nix-shell slint.nix --run 'cargo clean -p femtovg --target wasm32-unknown-unknown && cargo check -p app-web --target wasm32-unknown-unknown'
```

验完看日志里有没有 `Checking femtovg v0.26.0 (…#<锁里的哈希>)`。这条比上一条更会骗人:
`touch` 漏了至少还是原来的错,清错 target 却是一次看着通过的空验证。

顺手核对锁文件确实指向了新的来源:

```bash
grep -A2 'name = "femtovg"' Cargo.lock
```

## 三、什么时候该跑

没有固定周期。这几种情况值得跑一遍:

- 要向上游提 PR 之前。PR 分支必须从当前上游 master 拉,否则评审看到的是陈旧的基线。
- 上游合并了我们的补丁之后。留着重复的本地 commit 会在下次 rebase 时变成冲突。
- 需要上游的新功能或修复时。

落后多少个 commit 可以这样看:

```bash
cd ~/RustroverProjects/slint-fork && git fetch upstream && git rev-list --count dev..upstream/master
```

## 更新记录

- 2026-08-18 femtovg fork 退役:#302 前一天并入上游 master,slint fork 的 femtovg
  依赖改指 `femtovg/femtovg` git master(最新发布 0.26.0 早于合并,还回不了
  crates.io),`dev` 与 PR 两条分支删除,`dev` 留 tag 归档。slint 侧核对确认 wasm
  定帧、`Window::is_active()` 两条补丁上游仍没有,fork 继续;skia present 顺序那条
  已是死 commit(文件与上游逐字节相同)。宿主与 wasm 两侧验证均通过,wasm 日志里
  femtovg 确实从新来源重编。
- 2026-08-13 slint 的 dev 从落后 98 个 commit 合到上游 `e24172737`。present 顺序那条
  (slint#12861)上游已收下,冲突取上游那侧,补丁栈从四条回到三条。同时把 dev 的维护
  方式从「rebase + 强推」改成「merge + 快进推」,理由见第一节。
- 2026-08-08 slint 的 dev 从落后 288 个 commit 重建到上游 `a254143cb`,三条补丁全部
  cherry-pick 干净。femtovg 侧无事可做:`origin/master` 已是上游头,`dev` 领先它十个
  commit(femtovg#302 仍开着)。同时改掉本地路径验证的写法,理由见第 4 步。
- 2026-07-29 首版。此前 dev 落后上游 194 个 commit,这次重建后追平,丢弃两条已被
  上游覆盖的补丁,femtovg 的 pin 从 0.25.1 迁到 0.26.0。
