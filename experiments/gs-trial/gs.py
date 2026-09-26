# 公共件:.ply 读写、设计图坐标系下的相机、gsplat 渲染、设计图配准。
# 坐标系沿用 #140 的 sil.py:Z 朝上、脚底 z=0、脚底到耳尖 0.30m、头朝 -Y、x/y 以包围盒居中。
import numpy as np
import torch
import trimesh
from PIL import Image, ImageDraw
from plyfile import PlyData, PlyElement
from scipy import ndimage as nd

H = 0.30                      # 脚底到耳尖
PX = 2000.0                   # 设计视角的正交比例:像素/米(2 px/mm)

# 设计视角:相机所在方向。side 相机在 +X、画面右 = +Y、头在左;front 在 -Y;back 在 +Y。
# side_m 是侧面图水平镜像、相机放到 -X:猫大体左右对称,拿它补另一侧的观测(降权)。
VIEW_DIRS = {"side": (1, 0, 0), "front": (0, -1, 0), "back": (0, 1, 0), "side_m": (-1, 0, 0)}
VIEW_FILES = {"side": "side.png", "front": "front.png", "back": "back2.png", "side_m": "side.png"}


# ---------- 网格 ----------
def load_mesh(path):
    """和 sil.py / base.py 同一套归一化。"""
    m = trimesh.load(path, force="mesh")
    m.apply_transform(trimesh.transformations.rotation_matrix(np.pi / 2, [1, 0, 0]))
    lo, hi = m.bounds
    m.apply_scale(H / (hi[2] - lo[2]))
    lo, hi = m.bounds
    m.apply_translation([-(lo[0] + hi[0]) / 2, -(lo[1] + hi[1]) / 2, -lo[2]])
    return m


# ---------- 相机 ----------
def look_at(eye, target, up=(0, 0, 1)):
    """OpenCV 约定的 world->camera 4x4(x 右、y 下、z 朝前)。"""
    eye, target, up = (np.asarray(v, np.float64) for v in (eye, target, up))
    f = target - eye
    f /= np.linalg.norm(f)
    r = np.cross(f, up)
    if np.linalg.norm(r) < 1e-6:                      # 正上/正下看
        r = np.cross(f, (0, 1, 0))
    r /= np.linalg.norm(r)
    d = np.cross(f, r)
    M = np.eye(4)
    M[:3, :3] = np.stack([r, d, f])
    M[:3, 3] = -M[:3, :3] @ eye
    return M


def orbit(azim_deg, elev_deg):
    """相机所在方向:方位角 0 = 正面(-Y),90 = 猫的左侧(+X,即设计图侧面那一侧),180 = 背面。"""
    a, e = np.deg2rad(azim_deg), np.deg2rad(elev_deg)
    return np.array([np.sin(a) * np.cos(e), -np.cos(a) * np.cos(e), np.sin(e)])


class Cam:
    def __init__(self, view, K, W, Hh, model):
        self.view, self.K, self.W, self.H, self.model = view, K, W, Hh, model

    def project(self, pts):
        """世界点 -> 像素 (u, v) 和相机深度。"""
        pc = pts @ self.view[:3, :3].T + self.view[:3, 3]
        f, c = np.array([self.K[0, 0], self.K[1, 1]]), self.K[:2, 2]
        uv = pc[:, :2] * f + c if self.model == "ortho" else pc[:, :2] / pc[:, 2:3] * f + c
        return uv, pc[:, 2]


def ortho_cam(direction, center, half_w, half_h, px=PX, dist=2.0):
    """正交相机:看向 center,画幅 2*half_w x 2*half_h 米。"""
    view = look_at(np.asarray(center) + np.asarray(direction, np.float64) * dist, center)
    W, Hh = int(round(2 * half_w * px)), int(round(2 * half_h * px))
    return Cam(view, np.array([[px, 0, W / 2], [0, px, Hh / 2], [0, 0, 1]]), W, Hh, "ortho")


def persp_cam(direction, center, fov_deg, W, Hh, dist):
    view = look_at(np.asarray(center) + np.asarray(direction) * dist, center)
    f = Hh / 2 / np.tan(np.deg2rad(fov_deg) / 2)
    return Cam(view, np.array([[f, 0, W / 2], [0, f, Hh / 2], [0, 0, 1]]), W, Hh, "pinhole")


def design_cams(mesh, margin=0.03):
    """四个设计视角的正交相机,画幅按网格包围盒加边。"""
    lo, hi = mesh.bounds
    c = (lo + hi) / 2
    hx, hy, hz = (hi - lo) / 2 + margin
    half_w = {"side": hy, "side_m": hy, "front": hx, "back": hx}
    return {k: ortho_cam(VIEW_DIRS[k], c, half_w[k], hz) for k in VIEW_DIRS}


# ---------- 渲染 ----------
def activate(p):
    return dict(means=p["means"], quats=torch.nn.functional.normalize(p["quats"], dim=-1),
                scales=torch.exp(p["scales"]), opacities=torch.sigmoid(p["opacities"]), sh0=p["sh0"])


def render(p, cams, bg=1.0):
    """p: 原始参数;cams 同画幅、同投影模型。返回 rgb [C,H,W,3]、alpha [C,H,W,1]。"""
    from gsplat import rasterization
    g = activate(p)
    dev = g["means"].device
    views = torch.tensor(np.stack([c.view for c in cams]), dtype=torch.float32, device=dev)
    Ks = torch.tensor(np.stack([c.K for c in cams]), dtype=torch.float32, device=dev)
    rgb, alpha, _ = rasterization(
        g["means"], g["quats"], g["scales"], g["opacities"], g["sh0"], views, Ks, cams[0].W, cams[0].H,
        sh_degree=0, camera_model=cams[0].model, near_plane=0.01, far_plane=100.0)
    return (rgb + (1 - alpha) * bg).clamp(0, 1), alpha


# ---------- .ply(标准 3DGS 格式,只有 SH0) ----------
_ATTRS = ["x", "y", "z", "nx", "ny", "nz", "f_dc_0", "f_dc_1", "f_dc_2", "opacity",
          "scale_0", "scale_1", "scale_2", "rot_0", "rot_1", "rot_2", "rot_3"]


def save_ply(p, path):
    """p 是原始参数:log 尺度、logit 不透明度、四元数 wxyz。"""
    t = lambda k: p[k].detach().float().cpu().numpy()
    means = t("means")
    data = np.concatenate([means, np.zeros_like(means), t("sh0").reshape(-1, 3),
                           t("opacities").reshape(-1, 1), t("scales"), t("quats")], 1).astype(np.float32)
    el = np.empty(len(data), dtype=[(a, "f4") for a in _ATTRS])
    for i, a in enumerate(_ATTRS):
        el[a] = data[:, i]
    PlyData([PlyElement.describe(el, "vertex")]).write(path)


def load_ply(path, device="cuda"):
    v = PlyData.read(path)["vertex"]
    col = lambda *ks: torch.tensor(np.stack([np.asarray(v[k], np.float32) for k in ks], 1), device=device)
    return dict(means=col("x", "y", "z"), sh0=col("f_dc_0", "f_dc_1", "f_dc_2").reshape(-1, 1, 3),
                opacities=col("opacity")[:, 0], scales=col("scale_0", "scale_1", "scale_2"),
                quats=col("rot_0", "rot_1", "rot_2", "rot_3"))


def transform(p, R, s, t):
    """整体旋转 + 均匀缩放 + 平移:x' = s R x + t,四元数和尺度跟着变。"""
    import roma
    Rt = torch.tensor(R, dtype=torch.float32, device=p["means"].device)
    q = roma.rotmat_to_unitquat(Rt)                    # xyzw
    q = torch.cat([q[3:], q[:3]])                      # -> wxyz
    qs = torch.nn.functional.normalize(p["quats"], dim=-1)
    w1, v1, w2, v2 = q[0], q[1:], qs[:, :1], qs[:, 1:]
    quats = torch.cat([w1 * w2 - (v2 @ v1)[:, None],
                       w1 * v2 + w2 * v1 + torch.cross(v1.expand_as(v2), v2, dim=-1)], 1)
    tt = torch.tensor(t, dtype=torch.float32, device=Rt.device)
    return dict(p, means=s * p["means"] @ Rt.T + tt, scales=p["scales"] + float(np.log(s)), quats=quats)


# ---------- 设计图 ----------
def design_mask(img):
    """白底设计图的前景:最暗通道 < 232,闭运算后填洞,取最大连通块(同 sil.py)。"""
    fg = img.min(axis=2) < 232
    fg = nd.binary_fill_holes(nd.binary_closing(fg, iterations=3))
    lab, n = nd.label(fg)
    return lab == 1 + int(np.argmax(nd.sum(fg, lab, range(1, n + 1))))


def mesh_silhouette(mesh, cam):
    uv, _ = cam.project(mesh.vertices)
    im = Image.new("L", (cam.W, cam.H))
    d = ImageDraw.Draw(im)
    for t in uv[mesh.faces]:
        d.polygon([tuple(p) for p in t], fill=255)
    return np.asarray(im) > 0


def _paste(img, mask, s, dx, dy, W, Hh):
    """设计图按比例 s 缩放、左上角放到 (dx, dy),返回画布上的 rgb 和 mask。"""
    h, w = mask.shape
    nw, nh = max(1, round(w * s)), max(1, round(h * s))
    rgb = np.asarray(Image.fromarray(img).resize((nw, nh), Image.LANCZOS)).astype(np.float32) / 255
    mk = np.asarray(Image.fromarray(mask.astype(np.uint8) * 255).resize((nw, nh), Image.BILINEAR)) / 255.0
    out_rgb, out_mk = np.ones((Hh, W, 3), np.float32), np.zeros((Hh, W), np.float32)
    x0, y0 = int(round(dx)), int(round(dy))
    xs, ys, xe, ye = max(0, x0), max(0, y0), min(W, x0 + nw), min(Hh, y0 + nh)
    if xe > xs and ye > ys:
        out_rgb[ys:ye, xs:xe] = rgb[ys - y0:ye - y0, xs - x0:xe - x0]
        out_mk[ys:ye, xs:xe] = mk[ys - y0:ye - y0, xs - x0:xe - x0]
    return out_rgb, out_mk


def register_design(path, cam, ref_sil, mirror=False):
    """把设计图配到相机画布上:先按「整高 = 网格整高」定比例、脚底贴齐,
    再在 ±6% 比例、±40/±20px 平移里搜和网格轮廓 IoU 最大的那组。"""
    img = np.asarray(Image.open(path).convert("RGB"))
    if mirror:
        img = np.ascontiguousarray(img[:, ::-1])
    m = design_mask(img)
    ys, xs = np.where(m)
    img, m = img[ys.min():ys.max() + 1, xs.min():xs.max() + 1], m[ys.min():ys.max() + 1, xs.min():xs.max() + 1]
    rys, rxs = np.where(ref_sil)
    s0 = (rys.max() - rys.min() + 1) / m.shape[0]
    f = 4                                               # 在 1/4 分辨率上搜
    ref = ref_sil[::f, ::f]
    small_img, small_m = np.ascontiguousarray(img[::f, ::f]), m[::f, ::f]
    best = (-1.0, None)
    for s in s0 * np.linspace(0.94, 1.06, 13):
        w, h = m.shape[1] * s, m.shape[0] * s
        cx0, cy0 = (rxs.min() + rxs.max()) / 2 - w / 2, rys.max() + 1 - h
        for dx in range(-40, 41, 4):
            for dy in range(-20, 21, 4):
                _, mk = _paste(small_img, small_m, s, (cx0 + dx) / f, (cy0 + dy) / f, ref.shape[1], ref.shape[0])
                mk = mk > 0.5
                iou = (mk & ref).sum() / max(1, (mk | ref).sum())
                if iou > best[0]:
                    best = (iou, (s, cx0 + dx, cy0 + dy))
    s, dx, dy = best[1]
    rgb, mk = _paste(img, m, s, dx, dy, cam.W, cam.H)
    return rgb, mk, dict(iou=float(best[0]), scale=float(s), dx=float(dx), dy=float(dy))


def load_targets(mesh, design_dir):
    """四个设计视角的相机和配准后的目标图。"""
    cams = design_cams(mesh)
    out = {}
    for k, cam in cams.items():
        sil = mesh_silhouette(mesh, cam)
        rgb, mk, info = register_design(f"{design_dir}/{VIEW_FILES[k]}", cam, sil, mirror=(k == "side_m"))
        out[k] = dict(cam=cam, rgb=rgb, mask=mk, sil=sil, info=info)
    return out
