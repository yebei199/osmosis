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
| `fit.py` | 路线 B:受约束拟合。高模表面采样初始化、设计图投影上色,gsplat 对四个设计视角优化,高模 SDF 和中间视角轮廓当约束;`--prior` 拿 LGM 的对齐结果在中间视角当低频颜色先验 |
| `eval.py` | 固定机位出对比图:设计图三视角 + 脸部特写并排,外加两个没画过的角度;`--align` 把 LGM 的输出对齐进设计图坐标系 |

坐标系沿用 #140 的 `sil.py`:Z 朝上、脚底 z=0、脚底到耳尖 0.30m、头朝 -Y。

跑法(pc3):

```sh
. ~/ai3d/gs-trial/env.sh
cd ~/ai3d/gs-trial/LGM && python ../scripts/lgm_run.py ../out/lgm      # 约 10s,显存峰值约 8GiB
cd ~/ai3d/gs-trial/scripts
python eval.py ../out/lgm/hybrid.ply ../out/lgm/eval-hybrid.png --align   # 另存 eval-hybrid-aligned.ply
python fit.py ../out/fit2 --prior ../out/lgm/eval-hybrid-aligned.ply --max-scale 0.004 --max-aniso 5   # 约 4min,显存峰值不到 1GiB
python eval.py ../out/fit2/fit.ply ../out/fit2/eval.png
```

## 结论(2026-09-26,一轮加一次修正,限额用完)

对比图在 pc1 `~/ai3d/unicat/`:`gs-trial-1.png` 是主图(路线 B 修正后),`gs-trial-1-lgm-hybrid.png`、
`gs-trial-1-lgm-idream.png`、`gs-trial-1-fit-round1.png` 是另外三只;`.ply` 和数字在 `gs-trial/` 下。

**像不像**:设计图画过的三个视角像,没画过的角度不像。两条路线的毛病不一样。

- 路线 B(受约束拟合):侧、正、背三视角轮廓 IoU 0.95 / 0.96 / 0.93,毛的笔触、白围兜、白爪都在;
  6px/mm 的脸部特写里眼睛、鼻子、胡须都对得上。可一离开这三个机位,表面就碎成深色杂点,
  画里的毛流看不出来,腿断成几截,爪子糊成一团。
- 路线 A 的 LGM hybrid(四个槽放设计图):中间角度的体形和黑白分区是连贯的,但整只糊
  (LGM 输入只有 256px,出 2.5 万个高斯),脸部特写认不出五官;身长比设计图长一截,侧面 IoU 只有 0.70。
- 路线 A 的 LGM idream(ImageDream 自己补侧面):补出来的侧面短、没有尾巴、毛色发灰,侧面 IoU 0.30,不往下做。

**哪里漂、哪里有洞、哪里变形**(B 为主):

- 第一轮:胡须和白围兜被当成表面颜色贴到了头顶、背上,中间角度看是一道道白条;尾巴末段断开。
- 修正轮(把 LGM hybrid 当中间视角的低频颜色先验,单个高斯最长半轴压到 4mm、长短轴比压到 5):白条没了,
  但侧面图里鼻尖和尾梢外面多出几根深色细线,是正、背视角里侧着看不见的长条高斯。
- 两轮都有:侧面图里远侧那条后腿是虚的。设计图画的是迈步,高模是四腿直立,两者对不上的那部分拟合不出来。
- LGM:正面头两侧有条纹状的片子,爪子毛边发散。

**数量和体积**:

| | 高斯数 | `.ply` | 用时 | 显存峰值 |
|---|---|---|---|---|
| B 修正后 | 286,615 | 18.6 MiB(float32,只存 SH0) | 准备 26s + 拟合 195s(6000 步) | 0.84 GiB |
| LGM hybrid | 25,080 | 1.3 MiB | 加载 3~8s,ImageDream 约 4s,LGM 0.2s | 8.04 GiB |
| LGM idream | 34,442 | 1.8 MiB | 同上 | 同上 |

**我的判断**:三张设计图撑不起高斯在中间角度的样子。拟合那条路训练视角贴得越像,中间角度越碎;
生成那条路连贯,但分辨率和五官离设计图太远。这正是 issue 定的「不像就停」。高斯要是还想再试,只剩
「先有好看的源资产、渲一圈再蒸馏」那条,前提是 #140 的主线先出一个好看的模型。
