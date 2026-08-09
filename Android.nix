# Android APK 工具链的 nix-shell 形式 —— 用于替代 docker/Dockerfile 的
# NixOS 原生方案。容器里有的东西这里都有:Android SDK(platform 34,
# build-tools 34.0.0)、NDK r27、cargo-ndk、基于 JDK 17 的 gradle 8,
# 以及配好 Android target 的 rustup。
#
#   nix-shell Android.nix
#   cargo xtask android            # 等价于下面两步
#   cargo ndk -t arm64-v8a --platform 26 -o apps/android/gradle/app/src/main/jniLibs \
#       build -p app-android --lib --release
#   (cd apps/android/gradle && gradle --no-daemon assembleDebug)
#
# 体积很重(SDK+NDK 有几个 GB)且是 unfree 包,所以要手动 `nix-shell`,
# 不作为 direnv 的默认加载项。这里导入 nixpkgs 时接受了许可协议,
# androidenv 要求如此。
let
  pkgs = import <nixpkgs> {
    config = {
      allowUnfree = true;
      android_sdk.accept_license = true;
    };
  };

  # 固定版本号,与 Docker 构建器所用的版本保持一致。
  ndkVersion = "27.2.12479018";
  buildToolsVersion = "34.0.0";
  platformVersion = "34";

  android = pkgs.androidenv.composeAndroidPackages {
    cmdLineToolsVersion = "11.0";
    platformVersions = [ platformVersion ];
    buildToolsVersions = [ buildToolsVersion ];
    includeNDK = true;
    ndkVersions = [ ndkVersion ];
  };

  sdk = "${android.androidsdk}/libexec/android-sdk";
  ndkBin = "${sdk}/ndk/${ndkVersion}/toolchains/llvm/prebuilt/linux-x86_64/bin";
  # 与 xtask 的 MIN_SDK、gradle 的 minSdk 是同一个数。
  minSdk = "26";
in
pkgs.mkShell {
  buildInputs = [
    android.androidsdk
    (pkgs.gradle_8.override { java = pkgs.jdk17; })
    pkgs.jdk17
    pkgs.cargo-ndk
    pkgs.rustup
    pkgs.pkg-config
  ];

  ANDROID_HOME = sdk;
  ANDROID_SDK_ROOT = sdk;
  ANDROID_NDK_HOME = "${sdk}/ndk/${ndkVersion}";
  ANDROID_NDK_ROOT = "${sdk}/ndk/${ndkVersion}";
  JAVA_HOME = "${pkgs.jdk17}";

  # 依赖树里 blake3、audiopus_sys 这类带 C 代码的 crate 由 cc-rs 驱动编译,而 cc-rs 找
  # 交叉编译器的顺序是 CC_<target> → CC → 按三元组猜 `<triple>-clang`。前两条都不设就
  # 落到第三条,可 NDK r23 起不带 API level 的 `aarch64-linux-android-clang` 已经不存在
  # (这里只有 `…android26-clang` 这种)。而外层 devshell 通常导出 CC=gcc,那更糟:cc-rs
  # 拿宿主 gcc 去编 ARM NEON 代码,报的是 `arm_neon.h: No such file or directory`。
  # 目标专用变量优先级最高,设了两头都治。
  #
  # `cargo xtask android` 从不受影响 —— 它走 cargo-ndk,那个工具自己会设这些。踩坑的是
  # 直接 `cargo check --target …` 的路径,也就是 justfile 的 ci-cross。CI 里对应的是
  # .github/workflows/ci.yml 那个「确认 Android SDK 与 NDK 就位」step,本文件补齐的正是它。
  CC_aarch64_linux_android = "${ndkBin}/aarch64-linux-android${minSdk}-clang";
  AR_aarch64_linux_android = "${ndkBin}/llvm-ar";
  CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = "${ndkBin}/aarch64-linux-android${minSdk}-clang";

  # armv7 的 rust 三元组是 armv7-,NDK 的包装脚本却叫 armv7a-,两边对不上是常见的翻车点。
  CC_armv7_linux_androideabi = "${ndkBin}/armv7a-linux-androideabi${minSdk}-clang";
  AR_armv7_linux_androideabi = "${ndkBin}/llvm-ar";
  CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER = "${ndkBin}/armv7a-linux-androideabi${minSdk}-clang";

  CC_x86_64_linux_android = "${ndkBin}/x86_64-linux-android${minSdk}-clang";
  AR_x86_64_linux_android = "${ndkBin}/llvm-ar";
  CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER = "${ndkBin}/x86_64-linux-android${minSdk}-clang";

  # audiopus_sys 内嵌的 libopus 写着 cmake_minimum_required(VERSION <3.5),而本 shell
  # 里的 cmake 是 4.x,它已经删掉了对 3.5 以下的兼容,配置阶段直接报错退出。桌面端不
  # 受影响 —— 那边链的是系统 opus,只有交叉编译才会走内嵌的 cmake 构建。
  # 这个变量是 cmake 4.0 起官方给的过渡开关,等 audiopus_sys 上游修好就能删。
  CMAKE_POLICY_VERSION_MINIMUM = "3.5";

  shellHook = ''
    # `.envrc` 里 direnv 一进目录就加载 slint.nix(桌面工具链),而本 shell 是**叠**在
    # 那之上的,不是替换。它留下的 PKG_CONFIG_PATH 指着宿主机的 libopus;audiopus_sys
    # 的 build.rs 会问 pkg-config,于是把宿主机那份 .so 的路径当成链接搜索路径发出去,
    # aarch64 的链接器报「libopus.so is incompatible with aarch64linux」。
    #
    # 交叉编译时宿主机的库路径永远是错的,不是「这次不巧」。清掉之后 audiopus_sys 回落
    # 到内嵌的 cmake 构建 —— 上面 CMAKE_POLICY_VERSION_MINIMUM 那条注释描述的本来就是
    # 这条路径,只是它先撞上了宿主机的库,压根没走到。
    #
    # 这条错误只在**真的打 APK** 时才出现:`ci-cross` 与 CI 跑的都是 `cargo check`,
    # 而 check 不链接。所以它躲过了每一条自动化路径,直到有人等完三小时的冷编(见 #45)。
    unset PKG_CONFIG_PATH

    # AGP 会从 Maven 拉取预编译的 aapt2,但那个二进制在 NixOS 上跑不了;
    # 这里指向 androidenv build-tools 里的 aapt2,它已被 patchelf 过,可以运行。
    export GRADLE_OPTS="-Dorg.gradle.project.android.aapt2FromMavenOverride=${sdk}/build-tools/${buildToolsVersion}/aapt2"

    # cargo-ndk 交叉编译依赖 rustup 按 target 安装的 std;这里补上这些
    # target(已存在则是 no-op)。需要先装好默认工具链,并且要联网。
    rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android 2>/dev/null || \
      echo "note: run 'rustup default stable' first, then re-enter the shell to add Android targets"

    echo "Android toolchain ready: SDK $ANDROID_HOME, NDK ${ndkVersion}"
  '';
}
