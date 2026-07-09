# Android APK 工具链的 nix-shell 形式 —— 用于替代 docker/Dockerfile 的
# NixOS 原生方案。容器里有的东西这里都有:Android SDK(platform 34,
# build-tools 34.0.0)、NDK r27、cargo-ndk、基于 JDK 17 的 gradle 8,
# 以及配好 Android target 的 rustup。
#
#   nix-shell Android.nix
#   cargo ndk -t arm64-v8a --platform 26 -o android/app/src/main/jniLibs \
#       build --lib --release --no-default-features --features android
#   (cd android && gradle --no-daemon assembleDebug)
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

  shellHook = ''
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
