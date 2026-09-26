# 路线 B:受约束拟合。三张设计图(外加侧面镜像)配正交相机和前景 mask,Hunyuan 高模当几何先验。
# 用法: python fit.py <out_dir> [--n 300000] [--iters 6000] [--prior aligned.ply] [--max-scale 0.006] [--max-aniso 8]
# 输入: ~/ai3d/unicat/rest-hi.glb、~/ai3d/unicat/views-rest/*.png(只读)
# 产物: <out_dir>/fit.ply、targets.png(配准检查)、fit.json(数量、用时、显存峰值)
import argparse
import json
import os
import time

import numpy as np
import torch
import torch.nn.functional as F
from PIL import Image
from scipy import ndimage as nd
from scipy.spatial import cKDTree

import gs

ap = argparse.ArgumentParser()
ap.add_argument("out")
ap.add_argument("--mesh", default=os.path.expanduser("~/ai3d/unicat/rest-hi.glb"))
ap.add_argument("--designs", default=os.path.expanduser("~/ai3d/unicat/views-rest"))
ap.add_argument("--n", type=int, default=300_000)
ap.add_argument("--iters", type=int, default=6000)
ap.add_argument("--prior", help="已对齐到设计图坐标系的 .ply(eval.py --align 的产物),在中间视角当低频颜色先验")
ap.add_argument("--max-scale", type=float, default=0.006)
ap.add_argument("--max-aniso", type=float, default=8.0)
args = ap.parse_args()
os.makedirs(args.out, exist_ok=True)
dev = "cuda"
torch.manual_seed(0)
np.random.seed(0)
t0 = time.time()

# 设计视角权重:镜像侧面只是「猫大体对称」的猜测,降权
W_VIEW = {"side": 1.0, "front": 1.0, "back": 0.8, "side_m": 0.35}
BAND_OUT, BAND_IN = 0.015, 0.012     # 高斯中心离高模表面的容许带(米):外 15mm 给毛尖,内 12mm
MAX_SCALE = args.max_scale          # 单个高斯最长半轴超过它开始罚,压住中间视角里的长针

mesh = gs.load_mesh(args.mesh)
T = gs.load_targets(mesh, args.designs)
print("REGISTER", {k: v["info"] for k, v in T.items()}, flush=True)


def overlay(t):
    """配准检查:设计图压暗,高模轮廓叠红。"""
    o = t["rgb"] * 0.6
    o[..., 0] += t["sil"] * 0.4
    return Image.fromarray((np.clip(o, 0, 1) * 255).astype(np.uint8))


for k in ("side", "front", "back"):
    overlay(T[k]).save(f"{args.out}/register-{k}.png")

# ---------- 初始化:高模表面采样 ----------
pts, fidx = mesh.sample(args.n, return_index=True)
nrm = mesh.face_normals[fidx]
d_nn, _ = cKDTree(pts).query(pts, k=4)
spacing = d_nn[:, 1:].mean(1)


def init_colors():
    """按可见性把设计图投影到采样点上;没有任何视角看见的点取最近的已着色点。"""
    acc, wsum = np.zeros((len(pts), 3)), np.zeros(len(pts))
    for k, t in T.items():
        cam = t["cam"]
        uv, z = cam.project(pts)
        ui, vi = np.clip(uv[:, 0].astype(int), 0, cam.W - 1), np.clip(uv[:, 1].astype(int), 0, cam.H - 1)
        zbuf = np.full((cam.H, cam.W), np.inf)
        np.minimum.at(zbuf, (vi, ui), z)
        zbuf = nd.grey_erosion(zbuf, size=3)            # 点之间的缝隙别被当成「看得见」
        vis = z <= zbuf[vi, ui] + 0.004
        facing = np.clip(-(nrm @ (cam.view[2, :3])), 0, 1) ** 2
        w = vis * facing * (t["mask"][vi, ui] > 0.5) * W_VIEW[k]
        acc += w[:, None] * t["rgb"][vi, ui]
        wsum += w
    seen = wsum > 1e-3
    col = np.zeros((len(pts), 3))
    col[seen] = acc[seen] / wsum[seen, None]
    _, j = cKDTree(pts[seen]).query(pts[~seen])
    col[~seen] = col[seen][j]
    print(f"INIT colored {seen.mean():.1%} directly", flush=True)
    return col


col = np.clip(init_colors(), 0.02, 0.98)
P = {
    "means": torch.tensor(pts, dtype=torch.float32, device=dev),
    "scales": torch.tensor(np.log(np.repeat(spacing[:, None], 3, 1) * 0.8), dtype=torch.float32, device=dev),
    "quats": torch.tensor(np.tile([1.0, 0, 0, 0], (len(pts), 1)), dtype=torch.float32, device=dev),
    "opacities": torch.full((len(pts),), 2.0, device=dev),                 # sigmoid(2) ≈ 0.88
    "sh0": torch.tensor((col - 0.5) / 0.28209479177387814, dtype=torch.float32, device=dev).reshape(-1, 1, 3),
}
for v in P.values():
    v.requires_grad_(True)
LR = {"means": 1e-4, "scales": 5e-3, "quats": 1e-3, "opacities": 2.5e-2, "sh0": 2.5e-3}
opt = torch.optim.Adam([{"params": [P[k]], "lr": LR[k], "name": k} for k in P], eps=1e-15)

# ---------- 几何先验:高模的有符号距离场(体素 + 距离变换) ----------
PITCH = 0.002
vox = mesh.voxelized(PITCH).fill()
occ = vox.matrix
sdf = (nd.distance_transform_edt(~occ) - nd.distance_transform_edt(occ)) * PITCH
sdf_t = torch.tensor(sdf, dtype=torch.float32, device=dev)[None, None]     # [1,1,X,Y,Z]
vox_origin = torch.tensor(vox.transform[:3, 3], dtype=torch.float32, device=dev)
vox_shape = torch.tensor(occ.shape, dtype=torch.float32, device=dev)


def sdf_at(x):
    idx = (x - vox_origin) / PITCH                      # 体素索引坐标
    g = idx / (vox_shape - 1) * 2 - 1                   # [-1,1],grid_sample 的顺序是 (Z, Y, X) -> 反过来
    return F.grid_sample(sdf_t, g.flip(-1)[None, None, None], align_corners=True, padding_mode="border").view(-1)


# ---------- 中间视角的「不许有洞」约束:高模轮廓往里收 5px 的区域 alpha 要满 ----------
lo, hi = mesh.bounds
center = (lo + hi) / 2
R_half = float(np.linalg.norm(hi - lo) / 2) + 0.02
NOVEL = []
PRIOR = gs.load_ply(args.prior) if args.prior else None
with torch.no_grad():
    for az in range(0, 360, 20):
        for el in (-25, 10, 40, 70):
            cam = gs.ortho_cam(gs.orbit(az, el), center, R_half, R_half, px=1000.0)
            _, a = gs.render(P, [cam])
            inside = nd.binary_erosion(a[0, ..., 0].cpu().numpy() > 0.5, iterations=5)
            pr = None
            if PRIOR is not None:                          # 先验渲染缩到 1/8 只留低频颜色,alpha 够实的地方才算
                prgb, pa = gs.render(PRIOR, [cam])
                pr = (F.avg_pool2d(prgb.permute(0, 3, 1, 2), 8), F.avg_pool2d(pa.permute(0, 3, 1, 2), 8) > 0.9)
            NOVEL.append((cam, torch.tensor(inside, device=dev), pr))


def ssim(x, y):
    """x,y: [1,3,H,W]。11x11 高斯窗的 SSIM。"""
    g = torch.exp(-((torch.arange(11, device=x.device) - 5.0) ** 2) / (2 * 1.5 ** 2))
    g = (g / g.sum())
    win = (g[:, None] * g[None]).expand(3, 1, 11, 11)
    f = lambda z: F.conv2d(z, win, padding=5, groups=3)
    mx, my = f(x), f(y)
    sxx, syy, sxy = f(x * x) - mx ** 2, f(y * y) - my ** 2, f(x * y) - mx * my
    c1, c2 = 0.01 ** 2, 0.03 ** 2
    return (((2 * mx * my + c1) * (2 * sxy + c2)) / ((mx ** 2 + my ** 2 + c1) * (sxx + syy + c2))).mean()


TG = {k: (torch.tensor(t["rgb"], device=dev), torch.tensor(t["mask"], dtype=torch.float32, device=dev)) for k, t in T.items()}
t_setup = time.time() - t0
torch.cuda.reset_peak_memory_stats()
t1 = time.time()
for it in range(args.iters):
    frac = it / args.iters
    for g in opt.param_groups:                           # 位置学习率指数衰减到 1%
        if g["name"] == "means":
            g["lr"] = LR["means"] * 0.01 ** frac
    loss = 0.0
    for k, t in T.items():
        rgb, alpha = gs.render(P, [t["cam"]])
        tg, mk = TG[k]
        l_rgb = (rgb[0] - tg).abs().mean() * 0.8 + 0.2 * (1 - ssim(rgb[0].permute(2, 0, 1)[None], tg.permute(2, 0, 1)[None]))
        loss = loss + W_VIEW[k] * (l_rgb + 0.5 * (alpha[0, ..., 0] - mk).abs().mean())
    for cam, inside, pr in (NOVEL[i] for i in np.random.choice(len(NOVEL), 2, replace=False)):
        rgb, a = gs.render(P, [cam])
        loss = loss + 1.0 * (F.relu(0.98 - a[0, ..., 0]) * inside).sum() / inside.sum().clamp(min=1)
        if pr is not None:
            low = F.avg_pool2d(rgb.permute(0, 3, 1, 2), 8)
            loss = loss + 0.5 * ((low - pr[0]).abs() * pr[1]).sum() / pr[1].sum().clamp(min=1) / 3
    s = sdf_at(P["means"])
    loss = loss + 10.0 * ((F.relu(s - BAND_OUT) + F.relu(-s - BAND_IN)) / 0.005).pow(2).mean()
    sc = torch.exp(P["scales"])
    loss = loss + 10.0 * (F.relu(sc.max(1).values - MAX_SCALE) / 0.001).pow(2).mean()
    loss = loss + 0.01 * F.relu(sc.max(1).values / sc.min(1).values - args.max_aniso).mean()
    opt.zero_grad(set_to_none=True)
    loss.backward()
    opt.step()
    if it % 500 == 0 or it == args.iters - 1:
        print(f"it {it} loss {loss.item():.4f}", flush=True)
t_fit = time.time() - t1

with torch.no_grad():                                    # 几乎透明的高斯不影响画面,删掉
    keep = torch.sigmoid(P["opacities"]) > 0.02
    P = {k: v[keep].detach() for k, v in P.items()}
gs.save_ply(P, f"{args.out}/fit.ply")
info = dict(gaussians=int(keep.sum()), init=args.n, iters=args.iters, setup_s=round(t_setup, 1),
            fit_s=round(t_fit, 1), peak_mem_gib=round(torch.cuda.max_memory_reserved() / 2 ** 30, 2),
            ply_mib=round(os.path.getsize(f"{args.out}/fit.ply") / 2 ** 20, 1),
            register={k: v["info"] for k, v in T.items()})
json.dump(info, open(f"{args.out}/fit.json", "w"), indent=1)
print("FIT", json.dumps(info), flush=True)
