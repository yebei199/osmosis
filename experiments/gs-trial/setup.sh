#!/usr/bin/env bash
# 在 pc3 上搭 #148 的高斯试验环境:~/ai3d/gs-trial,和 Hunyuan 那个环境互不相干。
# 用法(在 pc3 上): bash setup.sh   —— 可重复跑,已有的步骤会跳过。
set -euo pipefail
ROOT=${GS_ROOT:-$HOME/ai3d/gs-trial}
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$ROOT" && cd "$ROOT"

# CUDA 12.8 工具链:nvcc 12.8 最高只认 gcc 14,系统 gcc 15 编不过,单独钉一份 gcc14。
# Blackwell(sm_120)从 CUDA 12.8 / PyTorch 2.7 起才支持。
if [ ! -e cuda-home ]; then
  NIXPKGS_ALLOW_UNFREE=1 nix build --impure --out-link cuda-home --expr '
    let p = import (builtins.getFlake "nixpkgs") { config.allowUnfree = true; }; c = p.cudaPackages_12_8;
    in p.symlinkJoin { name = "cuda128-home"; paths = p.lib.concatMap (x: x.all) [ c.cuda_nvcc c.cuda_cudart c.cuda_cccl ];
      postBuild = "ln -s lib $out/lib64"; }'
fi
[ -e gcc14 ] || nix build nixpkgs#gcc14 --out-link gcc14

[ -d .venv ] || uv venv -p 3.11 .venv
cp "$HERE/env.sh" "$ROOT/env.sh"
# shellcheck source=/dev/null
. "$ROOT/env.sh"
# pc3 走代理下 download.pytorch.org 只有约 1MB/s;阿里云镜像直连约 20MB/s
MIRROR=(--find-links https://mirrors.aliyun.com/pytorch-wheels/cu128/ --index-url https://mirrors.aliyun.com/pypi/simple/)
python -c 'import torch' 2>/dev/null ||
  env -u HTTPS_PROXY -u HTTP_PROXY uv pip install "${MIRROR[@]}" torch==2.7.0 torchvision==0.22.0

env -u HTTPS_PROXY -u HTTP_PROXY uv pip install "${MIRROR[@]}" setuptools wheel ninja numpy jaxtyping rich 'diffusers<0.30' 'transformers<4.46' 'huggingface_hub<0.26' accelerate safetensors \
  einops tyro kiui roma plyfile imageio trimesh scipy scikit-image pillow rtree

# gsplat 官方没有 pt27/cu128 的预编译轮子,从源码编(sm_120)
python -c 'import gsplat.csrc' 2>/dev/null ||
  uv pip install --no-build-isolation 'gsplat @ git+https://github.com/nerfstudio-project/gsplat.git@v1.5.3'

# LGM:推理只用它的网络和 ImageDream 管线;它的 diff-gaussian-rasterization 也得在 cu128 下重编
[ -d LGM ] || git clone --depth 1 https://github.com/3DTopia/LGM.git
[ -d diff-gaussian-rasterization ] ||
  git clone --recursive --depth 1 https://github.com/ashawkey/diff-gaussian-rasterization
python -c 'import diff_gaussian_rasterization' 2>/dev/null ||
  uv pip install --no-build-isolation ./diff-gaussian-rasterization

# 权重:pc3 走代理很慢还会卡死,直连 hf-mirror
( unset HTTPS_PROXY HTTP_PROXY https_proxy http_proxy ALL_PROXY all_proxy
  export HF_ENDPOINT=https://hf-mirror.com
  [ -f weights/LGM/model_fp16_fixrot.safetensors ] ||
    huggingface-cli download ashawkey/LGM model_fp16_fixrot.safetensors --local-dir weights/LGM
  [ -d weights/imagedream-ipmv-diffusers/unet ] ||
    huggingface-cli download ashawkey/imagedream-ipmv-diffusers --local-dir weights/imagedream-ipmv-diffusers )
echo SETUP_OK
