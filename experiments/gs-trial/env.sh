# 高斯试验的运行环境(pc3)。用法: . ~/ai3d/gs-trial/env.sh
GS_ROOT=${GS_ROOT:-$HOME/ai3d/gs-trial}
. "$GS_ROOT/.venv/bin/activate"
export CUDA_HOME=$GS_ROOT/cuda-home
export PATH=$CUDA_HOME/bin:$GS_ROOT/gcc14/bin:$PATH
export CC=$GS_ROOT/gcc14/bin/gcc CXX=$GS_ROOT/gcc14/bin/g++
export TORCH_CUDA_ARCH_LIST=12.0
# NixOS:torch 找驱动要 /run/opengl-driver/lib
export LD_LIBRARY_PATH=/run/opengl-driver/lib:$CUDA_HOME/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
