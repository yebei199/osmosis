# 复用项目开发 shell，追加本套件自己的隔离与软件音频依赖。
{
  pkgs ? import (builtins.fetchTarball {
    url = "https://releases.nixos.org/nixpkgs/nixpkgs-26.11pre1070934.1927682e0d80/nixexprs.tar.xz";
    sha256 = "sha256-sDGcZgdRVR58ceKz0RmkBtdUomBvu5vzbf6Aw5rUCo0=";
  }) { },
}:
let
  base = import ../../slint.nix { inherit pkgs; };
  acceptCheck = builtins.fetchurl {
    url = "https://git.cryptorust.uk/sibyl/nixos_config/raw/commit/ab618e265a9c85869c24f22c117363a5991c9836/home/features/development/ai_cli/skills/tdd/scripts/accept-check.py";
    sha256 = "a2224521120d8af0933cec4701f01f20b169198b8da890532d143035b272892a";
  };
in
base.overrideAttrs (old: {
  nativeBuildInputs =
    (old.nativeBuildInputs or [ ])
    ++ (with pkgs; [
      cargo
      cargo-nextest
      rustc
      uv
      python3
      passt
      pulseaudio
      alsa-plugins
      postgresql
      protobuf
      util-linux
      iproute2
      xorg-server
      mesa
      # 每晚全量生成覆盖地图；版本须与 rustc 自带的 LLVM 一致才读得懂 profraw。
      rustc.llvmPackages.llvm
    ]);
  shellHook = (old.shellHook or "") + ''
    export REMOTE_BEHAVIOR_ACCEPT_CHECK="${acceptCheck}"
    export REMOTE_BEHAVIOR_PYTHON="${pkgs.python3}/bin/python3"
    export SLINT_WGPU_CPU=1
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
        pkgs.stdenv.cc.cc.lib
      ]
    }:$LD_LIBRARY_PATH"
  '';
})
