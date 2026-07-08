# Android APK toolchain as a nix-shell — the NixOS-native replacement for
# docker/Dockerfile. Same pieces the container shipped: Android SDK (platform 34,
# build-tools 34.0.0), NDK r27, cargo-ndk, gradle 8 on JDK 17, and rustup with
# the Android targets.
#
#   nix-shell Android.nix
#   cargo ndk -t arm64-v8a --platform 26 -o android/app/src/main/jniLibs \
#       build --lib --release --no-default-features --features android
#   (cd android && gradle --no-daemon assembleDebug)
#
# Heavy (SDK+NDK are GBs) and unfree, so it is a manual `nix-shell`, not the
# direnv default. nixpkgs is imported here with the license accepted, which
# androidenv requires.
let
  pkgs = import <nixpkgs> {
    config = {
      allowUnfree = true;
      android_sdk.accept_license = true;
    };
  };

  # Pinned to match the versions the Docker builder shipped.
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
    # AGP fetches a prebuilt aapt2 from Maven that will not run on NixOS; point
    # it at the androidenv build-tools aapt2, which is patchelf'd to run here.
    export GRADLE_OPTS="-Dorg.gradle.project.android.aapt2FromMavenOverride=${sdk}/build-tools/${buildToolsVersion}/aapt2"

    # cargo-ndk cross-compiles with rustup's per-target std; add the targets
    # (no-op once present). Needs an installed default toolchain + network.
    rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android 2>/dev/null || \
      echo "note: run 'rustup default stable' first, then re-enter the shell to add Android targets"

    echo "Android toolchain ready: SDK $ANDROID_HOME, NDK ${ndkVersion}"
  '';
}
