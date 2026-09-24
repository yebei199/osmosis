# 本仓库唯一的开发 shell —— 桌面运行、服务端所需的 native 依赖全在这里。
#
# winit + skia 后端在构建期通过 yeslogic-fontconfig-sys(pkg-config)
# 链接系统 fontconfig,运行期则 dlopen wayland / libxkbcommon / libGL / X11 / vulkan。
# 裸的 NixOS shell 的 PKG_CONFIG_PATH/LD_LIBRARY_PATH 上没有这些东西,
# 所以 fontconfig 的 build.rs 会 panic。这个 shell 把两者都补上。
#
#   nix-shell slint.nix --run "cargo run -p app-desktop"
#   # 或者通过 direnv 自动加载(.envrc: `use nix slint.nix`)
#
# 曾经另有一个 render3d.nix,只比这里多一个 vulkan-loader、却少了 alsa 与 libopus ——
# 于是「带 3D 跑」和「有声音跑」是两个互斥的 shell,踩过一次。bevy 变成硬依赖之后
# 那个区分再无意义,合并成这一个。
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
    # 开了 unstable-wgpu-29 后 Slint 与 bevy 都走 wgpu,运行期要 dlopen libvulkan.so;
    # ICD 由系统的 /run/opengl-driver 提供。构建期不需要它(那是运行期 dlopen)。
    vulkan-loader
  ];
in
pkgs.mkShell {
  nativeBuildInputs = [
    pkgs.pkg-config
    # web 废弃(#110)后 wasm-bindgen-cli 已撤;复活时加回,版本要与 Cargo.lock 里的
    # wasm-bindgen 完全一致,否则 CLI 拒绝生成胶水代码。python3 给测试脚本起静态服务器。
    pkgs.python3
    # MPRIS 的测试自己起一条临时总线(`dbus-daemon --session`),免得往用户
    # 自己的会话总线上摆一个假播放器。zbus 本身是纯 Rust,不需要 libdbus ——
    # 这里要的只是那个可执行文件。
    pkgs.dbus
    # /download 的转码路跑真的 ffmpeg(测试里还用 ffprobe 验产出是不是合法 mp3)。
    # 不声明的话它只是碰巧在开发机的 PATH 上,而那种依赖坏掉时的现象是
    # 「换一台机器测试就红」,报错还只说找不到命令。
    pkgs.ffmpeg
  ];
  buildInputs = runtimeLibs;

  shellHook = ''
    export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath runtimeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  '';
}
