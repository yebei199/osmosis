# Slint 桌面开发 shell —— `cargo run --features desktop` 所需的 native 依赖。
#
# winit + femtovg 后端在构建期通过 yeslogic-fontconfig-sys(pkg-config)
# 链接系统 fontconfig,运行期则 dlopen wayland / libxkbcommon / libGL / X11。
# 裸的 NixOS shell 的 PKG_CONFIG_PATH/LD_LIBRARY_PATH 上没有这些东西,
# 所以 fontconfig 的 build.rs 会 panic。这个 shell 把两者都补上。
#
#   nix-shell slint.nix --run "cargo run --features desktop"
#   # 或者通过 direnv 自动加载(.envrc: `use nix slint.nix`)
#
# 固定用 <nixpkgs>,以跟随宿主机使用的同一个 channel。
{ pkgs ? import <nixpkgs> { } }:
let
  # winit/femtovg 在运行期 dlopen 这些库——必须出现在 LD_LIBRARY_PATH
  # 里,只是链接是不够的。fontconfig/freetype 在构建期还会被 pkg-config 找到。
  runtimeLibs = with pkgs; [
    fontconfig
    freetype
    wayland
    libxkbcommon
    libGL
    libx11
    libxcursor
    libxrandr
    libxi
    libxcb
    # rodio 走 cpal,linux 上构建期要 pkg-config 找到 alsa,运行期还要 dlopen 它。
    alsa-lib
    # opus crate 链接 libopus。同播的主控要把 PCM 重编码成 Opus 才能推上媒体轨,
    # 听众反向解回来 —— 没有纯 Rust 的编码器,这个 C 库绕不开。
    libopus
  ];
in
pkgs.mkShell {
  nativeBuildInputs = [
    pkgs.pkg-config
    # wasm 链路:版本必须与 Cargo.lock 里的 wasm-bindgen 完全一致,
    # 否则 CLI 直接拒绝生成胶水代码。python3 只用来起静态服务器。
    pkgs.wasm-bindgen-cli_0_2_126
    pkgs.python3
  ];
  buildInputs = runtimeLibs;

  shellHook = ''
    export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath runtimeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  '';
}
