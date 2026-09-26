# 路线 A:LGM(ImageDream 路径)。正面设计图 -> ImageDream 补四视图 -> LGM 出 splat。
# 另出一只 hybrid:四个输入槽里正面/背面/两侧换成真设计图(侧面镜像补另一侧),按生成视图的框对齐尺度。
# 用法(在 LGM 仓库根目录下): python <本目录>/lgm_run.py <out_dir>
# 产物: <out_dir>/{idream,hybrid}.ply、mv-{idream,hybrid}.png(喂给 LGM 的四张图)、lgm.json(用时、显存峰值)
import importlib.machinery
import json
import os
import sys
import time
import types

import numpy as np
import torch
import torch.nn.functional as F
import torchvision.transforms.functional as TF
from PIL import Image
from kiui.op import recenter
from safetensors.torch import load_file

# ImageDream 的 mv_unet 硬依赖 xformers,只用到 memory_efficient_attention;换成 torch 自带的 SDPA,
# 省掉一个要和 torch 2.7/cu128/sm_120 对版本的包。LGM 自己的注意力在 import 失败时本来就回落到普通实现。
_xf = types.ModuleType("xformers")
_xf.ops = types.ModuleType("xformers.ops")
_xf.__spec__ = importlib.machinery.ModuleSpec("xformers", None)   # diffusers 会 find_spec 它
_xf.ops.memory_efficient_attention = lambda q, k, v, attn_bias=None, op=None: F.scaled_dot_product_attention(q, k, v)
sys.modules.update({"xformers": _xf, "xformers.ops": _xf.ops})
sys.path.insert(0, os.getcwd())
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from core.models import LGM                              # noqa: E402  LGM 仓库
from core.options import config_defaults                 # noqa: E402
from mvdream.pipeline_mvdream import MVDreamPipeline     # noqa: E402
import gs                                                # noqa: E402

OUT = sys.argv[1]
ROOT = os.environ.get("GS_ROOT", os.path.expanduser("~/ai3d/gs-trial"))
DESIGNS = os.path.expanduser("~/ai3d/unicat/views-rest")
os.makedirs(OUT, exist_ok=True)
dev = "cuda"
MEAN, STD = (0.485, 0.456, 0.406), (0.229, 0.224, 0.225)

opt = config_defaults["big"]
opt.lambda_lpips = 0                                     # 推理用不上,省得去下 VGG
torch.cuda.reset_peak_memory_stats()
t0 = time.time()
model = LGM(opt)
model.load_state_dict(load_file(f"{ROOT}/weights/LGM/model_fp16_fixrot.safetensors", device="cpu"), strict=False)
model = model.half().to(dev).eval()
rays = model.prepare_default_rays(dev)
pipe = MVDreamPipeline.from_pretrained(f"{ROOT}/weights/imagedream-ipmv-diffusers", torch_dtype=torch.float16,
                                       trust_remote_code=True).to(dev)
t_load = time.time() - t0


def rgba(path, mirror=False):
    img = np.asarray(Image.open(path).convert("RGB"))
    if mirror:
        img = np.ascontiguousarray(img[:, ::-1])
    return np.concatenate([img, (gs.design_mask(img) * 255).astype(np.uint8)[..., None]], -1)


def white(im):
    im = im.astype(np.float32) / 255
    return im[..., :3] * im[..., 3:] + (1 - im[..., 3:])


def fg_box(im):
    ys, xs = np.where(im.min(-1) < 0.9)
    return xs.min(), ys.min(), xs.max() + 1, ys.max() + 1


def crop_fg(design_rgba):
    ys, xs = np.where(design_rgba[..., 3] > 0)
    return design_rgba[ys.min():ys.max() + 1, xs.min():xs.max() + 1]


def place(design_rgba, h, cy, size):
    """设计图按猫高 h 像素缩放,水平居中、竖直中心放在 cy,白底。四个槽共用同一个 h,尺度才一致。"""
    crop = crop_fg(design_rgba)
    nw = max(1, round(crop.shape[1] * h / crop.shape[0]))
    small = white(np.asarray(Image.fromarray(crop).resize((nw, h), Image.LANCZOS)))
    out = np.ones((size, size, 3), np.float32)
    x0, y0 = size // 2 - nw // 2, int(cy - h / 2)
    xs, xe, ys, ye = max(0, x0), min(size, x0 + nw), max(0, y0), min(size, y0 + h)
    out[ys:ye, xs:xe] = small[ys - y0:ye - y0, xs - x0:xe - x0]
    return out


def gaussians(mv):
    """mv: [4,H,W,3],顺序 = LGM 的方位角 0/90/180/270。"""
    x = torch.from_numpy(mv).permute(0, 3, 1, 2).float().to(dev)
    x = F.interpolate(x, size=(opt.input_size, opt.input_size), mode="bilinear", align_corners=False)
    x = torch.cat([TF.normalize(x, MEAN, STD), rays], 1)[None]
    with torch.no_grad(), torch.autocast("cuda", dtype=torch.float16):
        return model.forward_gaussians(x)


def save(g, name, mv):
    model.gs.save_ply(g, f"{OUT}/{name}.ply")
    Image.fromarray((np.concatenate(list(mv), 1) * 255).astype(np.uint8)).save(f"{OUT}/mv-{name}.png")


front = rgba(f"{DESIGNS}/front.png")
image = white(recenter(front, front[..., 3] > 0, border_ratio=0.2))
t1 = time.time()
mv = pipe("", image, guidance_scale=5.0, num_inference_steps=30, elevation=0)
mv = np.stack([mv[1], mv[2], mv[3], mv[0]], 0)                     # 同 LGM infer.py 的顺序
t_mv = time.time() - t1
t2 = time.time()
save(gaussians(mv), "idream", mv)
t_lgm = time.time() - t2

# hybrid:四个槽换成真设计图。共用一个猫高:取生成视图的猫高,但侧面(最长)得整只放进画幅 90% 以内;
# 竖直中心取生成视图的平均。90° 槽里的猫头朝哪边,就放侧面图还是它的镜像。
size = mv.shape[1]
side, side_m, back = rgba(f"{DESIGNS}/side.png"), rgba(f"{DESIGNS}/side.png", mirror=True), rgba(f"{DESIGNS}/back2.png")
boxes = [fg_box(v) for v in mv]
sc = crop_fg(side)
h = int(min(np.mean([b[3] - b[1] for b in boxes]), 0.9 * size * sc.shape[0] / sc.shape[1]))
cy = float(np.mean([(b[1] + b[3]) / 2 for b in boxes]))


def head_left(img):
    """最高的前景像素(耳尖)落在猫框左半边 -> 头朝左。"""
    fg = img.min(-1) < 0.9
    ys, xs = np.where(fg)
    return xs[ys.argmin()] < (xs.min() + xs.max()) / 2


s90 = side if head_left(mv[1]) == head_left(place(side, h, cy, size)) else side_m
s270 = side_m if s90 is side else side
hy = np.stack([place(front, h, cy, size), place(s90, h, cy, size), place(back, h, cy, size), place(s270, h, cy, size)])
save(gaussians(hy), "hybrid", hy)

info = dict(load_s=round(t_load, 1), imagedream_s=round(t_mv, 1), lgm_s=round(t_lgm, 2),
            peak_mem_gib=round(torch.cuda.max_memory_reserved() / 2 ** 30, 2), side_at_90="side" if s90 is side else "side_m",
            ply_mib={n: round(os.path.getsize(f"{OUT}/{n}.ply") / 2 ** 20, 1) for n in ("idream", "hybrid")})
json.dump(info, open(f"{OUT}/lgm.json", "w"), indent=1)
print("LGM", json.dumps(info), flush=True)
