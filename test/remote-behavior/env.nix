# 复用项目开发 shell，追加本套件自己的隔离与软件音频依赖。
{
  pkgs ? import (builtins.fetchTarball {
    url = "https://releases.nixos.org/nixpkgs/nixpkgs-26.11pre1070934.1927682e0d80/nixexprs.tar.xz";
    sha256 = "sha256-sDGcZgdRVR58ceKz0RmkBtdUomBvu5vzbf6Aw5rUCo0=";
  }) { },
}:
let
  base = import ../../slint.nix { inherit pkgs; };
in
base.overrideAttrs (old: {
  nativeBuildInputs =
    (old.nativeBuildInputs or [ ])
    ++ (with pkgs; [
      cargo
      rustc
      uv
      passt
      pulseaudio
      alsa-plugins
      postgresql
      protobuf
      util-linux
      xorg-server
      mesa
    ]);
  shellHook = (old.shellHook or "") + ''
    export ALSA_PLUGIN_DIR="${pkgs.alsa-plugins}/lib/alsa-lib"
    for remote_icd in ${pkgs.mesa.drivers}/share/vulkan/icd.d/lvp_icd*.json; do
      test -f "$remote_icd" || { echo "missing lavapipe ICD" >&2; return 2; }
      export VK_DRIVER_FILES="$remote_icd"
    done
    export LD_LIBRARY_PATH="${
      pkgs.lib.makeLibraryPath [
        pkgs.mesa
        pkgs.pulseaudio
        pkgs.alsa-plugins
      ]
    }:$LD_LIBRARY_PATH"
  '';
})
