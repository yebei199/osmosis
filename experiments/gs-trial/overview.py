# 六个视角并排的总览:上排是用户认可的三视图,下排是补画的三张。每张先裁到猫的外框再统一高度。
# 用法: python overview.py <三视图目录> <补画目录> <out.png> [--font 字体文件]
import argparse

import numpy as np
from PIL import Image, ImageDraw, ImageFont

ap = argparse.ArgumentParser()
ap.add_argument("ref")
ap.add_argument("extra")
ap.add_argument("out")
ap.add_argument("--font")
args = ap.parse_args()

ROWS = [[(f"{args.ref}/side.png", "侧面(原)"), (f"{args.ref}/front.png", "正面(原)"), (f"{args.ref}/back2.png", "背面(原)")],
        [(f"{args.extra}/front34.png", "前方四分之三略俯(补)"), (f"{args.extra}/back34.png", "后方四分之三·另一侧(补)"),
         (f"{args.extra}/top.png", "正上方俯视(补,转 90° 头朝左)", True)]]
HT, PAD = 480, 24
font = ImageFont.truetype(args.font, 30) if args.font else ImageFont.load_default()


def tile(path, label, rotate=False):
    im = Image.open(path).convert("RGB")
    if rotate:                                             # 俯视图是竖幅,转成横幅才和别的同高可读
        im = im.rotate(90, expand=True)
    ys, xs = np.where(np.asarray(im).min(axis=2) < 232)   # 白底上的猫外框
    # 框超出原图的部分 PIL 会填黑,所以夹在原图内
    im = im.crop((max(0, xs.min() - PAD), max(0, ys.min() - PAD),
                  min(im.width, xs.max() + PAD), min(im.height, ys.max() + PAD)))
    im = im.resize((round(im.width * HT / im.height), HT), Image.LANCZOS)
    d = ImageDraw.Draw(im)
    out = Image.new("RGB", (max(im.width, int(d.textlength(label, font=font)) + 20), HT + 50), "white")
    out.paste(im, ((out.width - im.width) // 2, 50))
    ImageDraw.Draw(out).text((10, 8), label, fill=(170, 30, 30), font=font)
    return out


rows = []
for row in ROWS:
    ims = [tile(*t) for t in row]
    r = Image.new("RGB", (sum(i.width for i in ims) + 16 * (len(ims) - 1), ims[0].height), "white")
    x = 0
    for i in ims:
        r.paste(i, (x, 0))
        x += i.width + 16
    rows.append(r)
W = max(r.width for r in rows)
sheet = Image.new("RGB", (W, sum(r.height for r in rows) + 16), (235, 235, 235))
y = 0
for r in rows:
    sheet.paste(r, ((W - r.width) // 2, y))
    y += r.height + 16
sheet.save(args.out)
print("OVERVIEW", args.out, sheet.size)
