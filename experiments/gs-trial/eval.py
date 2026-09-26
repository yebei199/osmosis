# 固定机位出图并和设计图并排:侧面、正面、背面、脸部特写(左设计图右高斯),
# 外加两个设计图没画过的角度(前方四分之三俯视、后方四分之三)。
# 用法: python eval.py <in.ply> <out.png> [--align]
#   --align:.ply 不在设计图坐标系里(LGM 的输出),先在 4 个朝上轴 x 4 个偏航里挑和设计图轮廓最像的一组,
#            再按不透明高斯的包围盒归一化到脚底 z=0、整高 0.30m。
# 产物: <out.png>,旁边的 <out>-views/ 放单张,<out>.json 记高斯数量、.ply 体积、设计视角轮廓 IoU;
#       --align 时另存对齐后的 <out>-aligned.ply
import argparse
import json
import os

import numpy as np
import torch
from PIL import Image, ImageDraw, ImageFont

import gs

ap = argparse.ArgumentParser()
ap.add_argument("ply")
ap.add_argument("out")
ap.add_argument("--align", action="store_true")
ap.add_argument("--mesh", default=os.path.expanduser("~/ai3d/unicat/rest-hi.glb"))
ap.add_argument("--designs", default=os.path.expanduser("~/ai3d/unicat/views-rest"))
args = ap.parse_args()
stem = os.path.splitext(args.out)[0]
os.makedirs(f"{stem}-views", exist_ok=True)

mesh = gs.load_mesh(args.mesh)
T = gs.load_targets(mesh, args.designs)
P = gs.load_ply(args.ply)


def iou(p, k):
    with torch.no_grad():
        _, a = gs.render(p, [T[k]["cam"]])
    a, m = a[0, ..., 0].cpu().numpy() > 0.5, T[k]["mask"] > 0.5
    return float((a & m).sum() / max(1, (a | m).sum()))


def normalize(p):
    """按不透明高斯的包围盒:整高 0.30m、脚底 z=0、x/y 居中。"""
    x = p["means"][torch.sigmoid(p["opacities"]) > 0.5]
    lo, hi = torch.quantile(x, 0.005, 0), torch.quantile(x, 0.995, 0)
    s = gs.H / float(hi[2] - lo[2])
    t = -s * torch.stack([(lo[0] + hi[0]) / 2, (lo[1] + hi[1]) / 2, lo[2]])
    return gs.transform(p, np.eye(3), s, t.cpu().numpy())


if args.align:
    ups = {"+y": np.array([[1, 0, 0], [0, 0, -1], [0, 1, 0]]), "-y": np.array([[1, 0, 0], [0, 0, 1], [0, -1, 0]]),
           "+z": np.eye(3), "-z": np.diag([1, -1, -1])}
    best = (-1, None)
    for un, U in ups.items():
        for yaw in (0, 90, 180, 270):
            c, s = np.cos(np.deg2rad(yaw)), np.sin(np.deg2rad(yaw))
            R = np.array([[c, -s, 0], [s, c, 0], [0, 0, 1]]) @ U
            q = normalize(gs.transform(P, R, 1.0, np.zeros(3)))
            score = iou(q, "side") + iou(q, "front")
            if score > best[0]:
                best = (score, (un, yaw, q))
    P = best[1][2]
    # 包围盒量出来的整高会被半透明的爪尖、耳尖带偏,再按轮廓 IoU 细调一次比例(以脚底中心为原点)和上下位置
    fine = (-1, None)
    for s in np.linspace(0.7, 1.15, 19):
        for dz in np.linspace(-0.05, 0.05, 11):
            q = gs.transform(P, np.eye(3), float(s), np.array([0, 0, dz]))
            score = iou(q, "side") + iou(q, "front")
            if score > fine[0]:
                fine = (score, (s, dz, q))
    print("ALIGN up", best[1][0], "yaw", best[1][1], "scale", round(fine[1][0], 3), "dz", fine[1][1],
          "score", round(fine[0], 3), flush=True)
    P = fine[1][2]
    gs.save_ply(P, f"{stem}-aligned.ply")                # 对齐后的一份,fit.py --prior 用

# ---------- 机位 ----------
lo, hi = mesh.bounds
center = (lo + hi) / 2
FACE_PX = 6000.0                                        # 脸部特写 6 px/mm
face_c = np.array([0.0, lo[1] + 0.05, 0.245])           # 头在 -Y 端,脸心大约离地 245mm
face_half = 0.065


def face_pair():
    """脸部特写:高斯按 6px/mm 直接渲;设计图在同比例的全身画布上配准后裁同一块。"""
    full = gs.ortho_cam(gs.VIEW_DIRS["front"], center, (hi[0] - lo[0]) / 2 + 0.03, (hi[2] - lo[2]) / 2 + 0.03, px=FACE_PX)
    rgb, _, _ = gs.register_design(f"{args.designs}/front.png", full, gs.mesh_silhouette(mesh, full))
    u, v = full.project(face_c[None])[0][0]
    r = int(face_half * FACE_PX)
    design = rgb[int(v) - r:int(v) + r, int(u) - r:int(u) + r]
    cam = gs.ortho_cam(gs.VIEW_DIRS["front"], face_c, face_half, face_half, px=FACE_PX)
    return design, cam


def shot(cam):
    with torch.no_grad():
        rgb, _ = gs.render(P, [cam])
    return rgb[0].cpu().numpy()


tiles = {}
for k in ("side", "front", "back"):
    tiles[f"design {k}"], tiles[f"gaussian {k}"] = T[k]["rgb"], shot(T[k]["cam"])
tiles["design face"], face_cam = face_pair()
tiles["gaussian face"] = shot(face_cam)
NOVEL = {"front 3/4 from above (not drawn)": (45, 35), "back 3/4 other side (not drawn)": (-135, 20)}
for name, (az, el) in NOVEL.items():
    tiles[name] = shot(gs.persp_cam(gs.orbit(az, el), center, 30, 900, 900, 0.95))

for name, im in tiles.items():
    Image.fromarray((np.clip(im, 0, 1) * 255).astype(np.uint8)).save(f"{stem}-views/{name.split(' (')[0].replace(' ', '_').replace('/', '')}.png")

# ---------- 拼图:每行统一高度 ----------
ROWS = [["design side", "gaussian side"], ["design front", "gaussian front", "design back", "gaussian back"],
        ["design face", "gaussian face", *NOVEL]]
HT = 560
try:
    font = ImageFont.load_default(size=26)
except TypeError:
    font = ImageFont.load_default()
rows = []
for row in ROWS:
    ims = []
    for name in row:
        im = Image.fromarray((np.clip(tiles[name], 0, 1) * 255).astype(np.uint8))
        im = im.resize((round(im.width * HT / im.height), HT), Image.LANCZOS)
        ImageDraw.Draw(im).text((10, 8), name, fill=(200, 30, 30), font=font)
        ims.append(im)
    canvas = Image.new("RGB", (sum(i.width for i in ims) + 8 * (len(ims) - 1), HT), (255, 255, 255))
    x = 0
    for i in ims:
        canvas.paste(i, (x, 0))
        x += i.width + 8
    rows.append(canvas)
W = max(r.width for r in rows)
out = Image.new("RGB", (W, sum(r.height for r in rows) + 8 * (len(rows) - 1)), (235, 235, 235))
y = 0
for r in rows:
    out.paste(r, ((W - r.width) // 2, y))
    y += r.height + 8
out.save(args.out)

info = dict(ply=args.ply, gaussians=int(P["means"].shape[0]), ply_mib=round(os.path.getsize(args.ply) / 2 ** 20, 1),
            iou={k: round(iou(P, k), 3) for k in ("side", "front", "back")})
json.dump(info, open(f"{stem}.json", "w"), indent=1)
print("EVAL", json.dumps(info), flush=True)
