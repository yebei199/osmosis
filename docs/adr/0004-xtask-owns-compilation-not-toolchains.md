# xtask 拥有编译逻辑,但工具链供给仍归 nix 与 docker

构建逻辑写在 `xtask/`(一个零依赖的 Rust bin,经 cargo alias 暴露为 `cargo xtask`),
而 Android SDK/NDK/gradle 的**获取**仍由 `Android.nix` 与 `docker/Dockerfile` 负责。
`justfile` 保留为入口目录,recipe 体只是转调 `cargo xtask`。

## 为什么不让 xtask 也接管容器与 nix

xtask 是一个 Rust 程序:它得先有 rustc 才能跑。而"怎么把 Android SDK 弄到这台机器上"
恰恰是 nix 与 docker 在解决的问题。**xtask 无法自举出自己的工具链。**

强行让它 `docker run`,就是让 Rust 去拼接挂载点、uid、缓存卷的命令行字符串 ——
那是 shell 比 Rust 好写的东西。所以外层调用形态是:

```sh
nix-shell Android.nix --run 'cargo xtask android'
docker run ... cargo xtask android
```

## 为什么不继续用 shell

`scripts/build-apk.sh` 里约九成是"调外部工具 + 拷文件",shell 的主场。搬到 Rust 里
换来的只有两处正确性,但这两处都是安静地出错的那种:

- **ABI → target triple 的映射**从 `case` 变成 `match`,漏掉一个 ABI 编译期就报错,
  而不是产出一个装上去闪退的 APK。
- **platform jar 的选择**从 `ls | sort -V | tail -1` 变成按 API level 数值取最大,
  `android-9` 不会再赢过 `android-34`。

顺带,这些逻辑现在有单元测试。

代价是每一条 `cp` 都从一行变成三行。这笔账在只有一个端的时候是亏的;六个端、
每端一套产物路径之后才划算 —— 这也是为什么这一步排在结构重构之后,而不是之前。

## 保留 just

`just` 是命令目录,`just --list` 就是文档;`xtask` 是实现。换实现不换入口。
`just dev` / `install-apk` / `adb-reverse` / `serve-apk` 这类"一行命令包一下 adb 或
miniserve"的 recipe 不进 xtask —— 把它们包成 Rust 子命令是纯亏。
