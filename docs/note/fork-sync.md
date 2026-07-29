# 手动同步 fork

本项目的 slint 走 `yebei199/slint` 的 `dev` 分支,它又把 femtovg 指到
`yebei199/femtovg` 的一个分支。两层 fork 都不自动跟进上游,需要时按本文跑一遍。

不上定时任务是刻意的:`master` 要保持从上游**纯快进**,而 GitHub 的定时 workflow
只从默认分支读取,往 master 塞一个上游没有的 workflow 文件就把快进本身破坏了。

## 一、fork 的分支各是什么

`yebei199/slint`:

| 分支 | 内容 | 维护方式 |
|---|---|---|
| `master` | 上游 master 的镜像,零本地改动 | 纯快进 |
| `dev` | 上游 master + 四条本地补丁,本项目实际依赖 | rebase,强推 |
| `fix/*` | 向上游提 PR 的分支,各自从上游 master 拉 | 只在 PR 需要时 rebase |
| `backup/dev-pre-rebase` | 上一次 rebase 前的 dev,出事能回滚 | 每次 rebase 前更新 |

`yebei199/femtovg`:

| 分支 | 内容 | 维护方式 |
|---|---|---|
| `master` | 上游 master 的镜像,零本地改动 | 纯快进 |
| `dev` | 上游 master + femtovg#302 的十个 commit,slint fork 实际依赖 | 重建,强推 |
| `pr/wgpu-per-draw-allocations` | femtovg#302 的 PR 分支,不要 rebase(会打乱评审视图) | 只在 PR 需要时动 |

两个 fork 用同一套角色:`master` 镜像上游,`dev` 承载全部本地改动,PR 从 `dev` 拆出去
单独开分支,上游收下多少就从 `dev` 撤掉多少。PR 一旦合并或关闭,对应分支就该删,内容
自然留在上游代码或 PR 正文里。

`dev` 这个名字是刻意的。曾经叫 `pin/femtovg-0.26-wgpu-perf`,版本号写进分支名意味着
femtovg 每升一次版都要改名,还要跟着改 slint fork 里的 `branch =`。固定名字一次到位。

四条补丁分别是什么、各自的上游去向,写在 `Cargo.toml` 的 `[patch.crates-io]` 上方。

## 已删除的分支

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

删分支前先确认它的内容在别处能找回。两处经验:

探针那条差点漏掉。结论早就写下了,但复现步骤当时指向分支本身,那 11 行补丁只存在于
那条分支上。

判断「上游是否已经收下」不能用 `git rev-list --count upstream/master..branch`。squash
合并会换掉哈希,分支看上去永远领先。要按内容查:去上游代码里找那个函数、那个文件、
那个符号在不在。

## 二、同步步骤

### 1. master 快进

```bash
cd ~/RustroverProjects/slint-fork
git fetch upstream
git push origin upstream/master:refs/heads/master
```

`push-guard` 会拦主分支,这是设计如此。确认要更新后重跑同一条命令。

### 2. femtovg 的 dev 重建

只在上游 femtovg 有新版本、或 #302 有新 commit 时需要。

```bash
cd ~/RustroverProjects/femtovg-fork
git fetch upstream
git checkout -B dev upstream/master
git cherry-pick <#302 分支的第一个 commit>^..<最后一个 commit>
git push --force-with-lease origin dev
```

不要直接 rebase `pr/wgpu-per-draw-allocations`:那是开着的 PR 分支,强推会打乱
评审视图和行内评论。`dev` 是它的消费副本,两者分开。

### 3. dev 重建

先备份,再从上游重新长出来:

```bash
cd ~/RustroverProjects/slint-fork
git branch -f backup/dev-pre-rebase dev
git push origin backup/dev-pre-rebase
git checkout -B rebuild/dev upstream/master
git cherry-pick <四条补丁的 commit>
```

逐条 cherry-pick 而不是 `git rebase`,是为了在过程中对每条补丁问一遍「上游是不是
已经自己修了」。判断方法:去上游 master 里找对应的代码,看那个缺陷还在不在。这次
就是这样丢掉两条的(一条上游合了我们的 PR,一条被 femtovg 新版本顶掉)。

### 4. 验证后再推

验证要在推之前做,办法是把本项目的 patch 临时指向本地路径:

```toml
[patch.crates-io]
slint = { path = "../slint-fork/api/rs/slint" }
slint-build = { path = "../slint-fork/api/rs/build" }
```

用相对路径,绝对路径会把本机用户名和目录结构写进仓库。

```bash
nix-shell slint.nix --run 'cargo check --workspace && cargo test -p ui -p app-desktop -p render3d -p app-core -p audio'
```

`cargo check --workspace --all-features` 不能用:那会把 `mcp` 也一起开进来,而它是个
调试后门(理由见 `apps/desktop/Cargo.toml` 的 feature 注释)。

通过之后还原 `Cargo.toml`,推 dev:

```bash
cd ~/RustroverProjects/slint-fork
git push --force-with-lease origin rebuild/dev:dev
git checkout dev && git reset --hard origin/dev && git branch -D rebuild/dev
```

### 5. 本项目跟进锁文件

```bash
cargo update -p slint -p slint-build
```

**每个**被 patch 的包都要点名。cargo 不会自动放弃锁里已有的 registry 版本,少写一个
就静默变成 `patch.unused`,补丁不进产物还不报错。

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

- 2026-07-29 首版。此前 dev 落后上游 194 个 commit,这次重建后追平,丢弃两条已被
  上游覆盖的补丁,femtovg 的 pin 从 0.25.1 迁到 0.26.0。
