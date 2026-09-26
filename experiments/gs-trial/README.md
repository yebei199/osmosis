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
| `lgm_run.py` | 路线 A:LGM。`idream` 由 ImageDream 从正面图补四视图;`hybrid` 四个槽直接放设计图(侧面镜像补另一侧) |
| `fit.py` | 路线 B:受约束拟合。高模表面采样初始化、设计图投影上色,gsplat 对四个设计视角优化,高模 SDF 和中间视角轮廓当约束 |

坐标系沿用 #140 的 `sil.py`:Z 朝上、脚底 z=0、脚底到耳尖 0.30m、头朝 -Y。

跑法(pc3):

```sh
. ~/ai3d/gs-trial/env.sh
cd ~/ai3d/gs-trial/LGM && python ../scripts/lgm_run.py ../out/lgm      # 约 10s,显存峰值约 8GiB
cd ~/ai3d/gs-trial/scripts && python fit.py ../out/fit1                # 约 4min,显存峰值不到 1GiB
```
