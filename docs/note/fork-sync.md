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

| 分支 | 内容 |
|---|---|
| `pin/femtovg-0.26-wgpu-perf` | femtovg master + femtovg#302 的十个 commit,本项目实际依赖 |
| `pr/wgpu-per-draw-allocations` | femtovg#302 的 PR 分支,不要 rebase(会打乱评审视图) |

四条补丁分别是什么、各自的上游去向,写在 `Cargo.toml` 的 `[patch.crates-io]` 上方。

## 二、同步步骤

### 1. master 快进

```bash
cd ~/RustroverProjects/slint-fork
git fetch upstream
git push origin upstream/master:refs/heads/master
```

`push-guard` 会拦主分支,这是设计如此。确认要更新后重跑同一条命令。

### 2. femtovg 的 pin 分支重建

只在上游 femtovg 有新版本、或 #302 有新 commit 时需要。

```bash
cd ~/RustroverProjects/femtovg-fork
git fetch upstream origin
git checkout -B pin/femtovg-0.26-wgpu-perf upstream/master
git cherry-pick <#302 分支的第一个 commit>^..<最后一个 commit>
git push --force-with-lease origin pin/femtovg-0.26-wgpu-perf
```

分支名里的版本号要跟着 femtovg 的实际版本改。改名之后,slint fork 里
`internal/renderers/femtovg/Cargo.toml` 的 `branch =` 也要跟着改。

不要直接 rebase `pr/wgpu-per-draw-allocations`:那是开着的 PR 分支,强推会打乱
评审视图和行内评论。pin 分支是它的消费副本,两者分开。

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
nix-shell slint.nix --run 'nix-shell render3d.nix --run "cargo check --workspace && cargo check -p app-desktop --features bevy-3d && cargo test -p ui -p app-desktop -p render3d -p app-core -p audio"'
```

`cargo check --workspace --all-features` 不能用:`app-web` 的 `repro` 与 `bevy-3d`
两个 feature 互斥,各自定义一个 `start`,全开必然撞名。

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
