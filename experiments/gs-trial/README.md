# gs-trial

#148 的 3D 高斯泼溅(3DGS)静态试验:从设计图生成的静态高斯猫,在设计图视角和设计图没画过的
中间视角,像不像设计图。只回答这一个问题;骨骼驱动、Bevy/安卓渲染、动画都不在这里。

计算全在 pc3(RTX 5080),环境在 `~/ai3d/gs-trial`,和 Hunyuan 那个环境互不相干。
输入是 #140 的设计图 `~/ai3d/unicat/views-rest/{side,front,back2}.png` 和 Hunyuan 高模
`~/ai3d/unicat/rest-hi.glb`,只读不改。

| 文件 | 作用 |
|---|---|
| `setup.sh` | 搭环境:nix 出 CUDA 12.8 + gcc14,uv 出 Python 3.11 + PyTorch 2.7.0/cu128,源码编 gsplat 和 LGM 的光栅器,下权重 |
| `env.sh` | 运行前 `. ~/ai3d/gs-trial/env.sh` |
| `gs.py` | 公共件:坐标系、相机、gsplat 渲染、`.ply` 读写、设计图配准 |

坐标系沿用 #140 的 `sil.py`:Z 朝上、脚底 z=0、脚底到耳尖 0.30m、头朝 -Y。
