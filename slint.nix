# Slint desktop dev shell — native deps for `cargo run --features desktop`.
#
# The winit + femtovg backend links system fontconfig via yeslogic-fontconfig-sys
# (pkg-config at build time) and dlopen's wayland / libxkbcommon / libGL / X11 at
# runtime. A bare NixOS shell has none of these on PKG_CONFIG_PATH/LD_LIBRARY_PATH,
# so the fontconfig build.rs panics. This shell supplies both.
#
#   nix-shell slint.nix --run "cargo run --features desktop"
#   # or automatically via direnv (.envrc: `use nix slint.nix`)
#
# Pinned to <nixpkgs> so it tracks the same channel the host uses.
{ pkgs ? import <nixpkgs> { } }:
let
  # dlopen'd at runtime by winit/femtovg — must be on LD_LIBRARY_PATH, not just
  # linked. fontconfig/freetype are also found by pkg-config at build time.
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
  ];
in
pkgs.mkShell {
  nativeBuildInputs = [ pkgs.pkg-config ];
  buildInputs = runtimeLibs;

  shellHook = ''
    export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath runtimeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  '';
}
