# release —— 把一个已发布的 release 推到所有设备

`v*` tag 推上去以后,CI(`.github/workflows/release.yml`)只出桌面二进制与服务端镜像。
剩下的事归这里:安卓 APK、装到用户的设备、抬 nixos_config、给 infra 一个镜像引用。

```sh
just rollout            # 最新的 release
just rollout v0.1.15    # 指定 tag
```

**不管的**:打 tag、出桌面二进制与镜像(CI 管)、`nixos-rebuild`(用户自己跑)、
改 infra 仓库(交给 infra 会话)。

## 四步

每步独立,某步失败只进结尾汇总,不拦后面的;有失败时退出码非零。

1. **APK**。Release 上已有本版 APK 就下载并校验,否则 ssh 到 `ROLLOUT_BUILD_HOST`(默认
   pc3)跑 [`remote-build.sh`](remote-build.sh):在 `~/.cache/osmosis-release` 的专用 clone
   里 checkout 这个 tag、编 release APK(烘 `api_base`)、`scp` 取回。签名证书对不上约定值
   就停,不上传、不装机。通过后 `gh release upload` 补两份资产(不 recreate release)。
2. **设备**。`adb devices` 里型号在 `rollout_models` 名单上的逐台 `adb install -r`,
   装完从设备读回 `base.apk` 的 sha256 与资产比对。名单外的设备(开发机)不碰;名单上
   不在线的跳过并在汇总里列出。设了 `ROLLOUT_ADB_HOSTS`(IP 空格分隔)时,先对不在线的
   主机 `nmap` 扫 adb 端口逐个 `adb connect`。**只装 release、只 `-r` 覆盖,从不卸载、
   不清数据。**
3. **nixos_config**(`NIXOS_CONFIG`,默认 `~/nixos_config`)。工作树不干净就不动。
   `pull --ff-only` 后,三个桌面资产的哈希取自 `sha256sums.txt`,逐个
   `nix store prefetch-file` 实取核对,全对才改 `home/features/desktop/osmosis.nix` 的
   `version` 与哈希,提交并推送。已是本版就不提交。
4. **服务端镜像**。打印 `ghcr.io/yebei199/osmosis-server:<ver>@sha256:<index digest>`,
   并核对 index 里有 arm64。

## 签名

release APK 用的是 gradle 的 debug 构建类型,签名密钥是 pc1 当初的
`~/.android/debug.keystore`(#127 评论)。换一把 key 签出来的包覆盖安装会
`INSTALL_FAILED_UPDATE_INCOMPATIBLE`,只能卸载 —— 用户会掉登录。所以:

- 构建机上这把 key 放在 `~/.config/osmosis/release.keystore`(chmod 600,**不进版本库、
  不进 issue 与提交信息**)。`remote-build.sh` 经 `ORG_GRADLE_PROJECT_android.injected.signing.*`
  把它交给 gradle,任何 tag 都生效,不依赖该 tag 的 gradle 配置。缺这个文件就拒绝构建。
- 约定证书指纹写在 justfile 的 `release_cert_sha256`。`rollout` 对每个 APK(新编的、
  从 Release 下的)都用 apksigner 核一遍。
- 这把 key 丢了就再也签不出能覆盖安装的包。pc1 与 pc3 各有一份。

## APK 资产约定(稳定,下载 APK 的一方照此找资产、验哈希)

每个 release 上补两份资产,名字固定,`<ver>` 是不带 `v` 的版本号(tag `v0.1.15` → `0.1.15`):

| 资产 | 内容 |
|---|---|
| `osmosis-android-arm64-<ver>.apk` | arm64-v8a 的 release APK,连生产后端,证书指纹 = `release_cert_sha256` |
| `osmosis-android-arm64-<ver>.apk.sha256` | 一行 `sha256sum` 格式:`<64 位小写十六进制>  osmosis-android-arm64-<ver>.apk` |

- 校验文件单独一份,不并进 `sha256sums.txt`:后者由 CI 生成,补 APK 时改它就得 `--clobber`
  一个已发布的资产。两份 APK 资产上传后同样不再覆盖。
- APK 内的 `versionName` = workspace 版本(同 `<ver>`),`versionCode` = `major*1000000 +
  minor*1000 + patch`(0.1.15 → 1015),由 `cargo xtask android` 传给 gradle。v0.1.14 及以前
  的 APK 里仍是 `0.1.0` / `1`。
- 找「最新版」用 GitHub 的 latest release;某个 release 可能还没补上 APK(构建要一小时),
  消费方要容忍资产缺席。

## 配置

justfile 头部:`api_base`、`rollout_models`(设备型号)、`release_cert_sha256`。
环境变量覆盖:`ROLLOUT_BUILD_HOST`、`ROLLOUT_MODELS`、`ROLLOUT_ADB_HOSTS`、`NIXOS_CONFIG`、
`ROLLOUT_DIR`(产物落点,默认 `dist/rollout`)。

回归检查:[`test/rollout.sh`](../test/rollout.sh),假的 gh / ssh / adb / nix / docker
顶在 PATH 前面,几秒跑完。改了这里就跑一遍。
