# 桌面 3D 开发 shell —— 在 slint.nix 的基础上补 vulkan-loader。
#
# 开了 unstable-wgpu-29 后,Slint 与 bevy 都走 wgpu,运行期要 dlopen libvulkan.so;
# slint.nix 原本只给 GL/X11/wayland,不含 vulkan-loader,裸跑会找不到 ICD 而回退失败。
# 构建期不需要 vulkan(那是运行期 dlopen),所以纯编译用 slint.nix 也行,本 shell 供跑起来用。
#
#   nix-shell render3d.nix --run "cargo run -p app-desktop --features bevy-3d"
{ pkgs ? import <nixpkgs> { } }:
let
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
    # wgpu 的 vulkan 后端在运行期 dlopen 它;ICD 由系统的 /run/opengl-driver 提供。
    vulkan-loader
  ];
in
pkgs.mkShell {
  nativeBuildInputs = [ pkgs.pkg-config ];
  buildInputs = runtimeLibs;
  shellHook = ''
    export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath runtimeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  '';
}
