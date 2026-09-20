# 服务端镜像:发版到部署

release.yml 的 `server-image` job 在每次 `v*` tag 上把 arm64 镜像推到
`ghcr.io/yebei199/osmosis-server`,digest 写进 GitHub Release 说明。之前这一段
是手工的:2026-09-19 一天里主路由手工跑了四次 `docker buildx build` + `push`,
一次因为本机磁盘打满导致 `docker inspect` 回空,空 digest 被错误地钉进过
infra 清单(见 issue #101)。CI 化之后,本机不再需要 docker 构建。

## 三步

1. 打 tag(`git tag vX.Y.Z && git push origin vX.Y.Z`)。CI 并行跑出桌面产物
   与 arm64 服务端镜像,Release 说明里附一行 digest,形如:

   ```
   ghcr.io/yebei199/osmosis-server@sha256:...
   ```

2. 把这一行抄进 `infra/apps/music/k8s/osmosis.yaml` 的镜像字段,提交到
   infra 仓库。

3. `kubectl apply` 那份清单,确认 `/health` 返回 200、容器里 `ffmpeg -version`
   能跑通。

## 为什么不自动跑第 2、3 步

infra 是另一个仓库的写权限,而部署要人看一眼——这两条本身就是拦住自动化的
理由,不是能力不够。CI 只做它能安全做、也最容易做错的那部分:构建与推送。

## workflow_dispatch 不建 Release

不打 tag 手动跑这个 workflow,`server-image` job 照常构建推送镜像(tag 用
`dispatch` 占位),但 `release` job 的 `if` 条件只认 `push` + `refs/tags/*`,
不会创建 Release、也不会把 digest 写进任何地方——要拿 digest 得去 Actions
日志里的 `build_push` 步骤输出找。这与 `build` job 现有的 push-only Release
是同一条约定。
