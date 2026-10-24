"""确定性像素比较；正式基准只能经显式命令更新。"""

import argparse
import shutil
from pathlib import Path

from PIL import Image, ImageChops


def compare(actual, baseline, diff, *, channel_threshold=8, ratio_threshold=0.001):
    """任一通道差严格大于阈值算变化，变化占比严格超限才失败。"""
    actual, baseline, diff = Path(actual), Path(baseline), Path(diff)
    if not baseline.is_file():
        raise AssertionError(f"missing baseline: {baseline}; use explicit update")
    with Image.open(actual) as source, Image.open(baseline) as reference:
        image, expected = source.convert("RGBA"), reference.convert("RGBA")
    diff.parent.mkdir(parents=True, exist_ok=True)
    if image.size != expected.size:
        canvas = Image.new(
            "RGBA",
            (max(image.width, expected.width), max(image.height, expected.height)),
            "magenta",
        )
        canvas.paste(image, (0, 0))
        canvas.save(diff)
        raise AssertionError(f"screenshot size changed: {expected.size} -> {image.size}")
    delta = ImageChops.difference(image, expected)
    changed = sum(max(pixel) > channel_threshold for pixel in delta.getdata())
    ratio = changed / (image.width * image.height)
    if ratio > ratio_threshold:
        delta.convert("RGB").save(diff)
        raise AssertionError(f"changed pixel ratio {ratio:.6f} > {ratio_threshold}; diff: {diff}")


def update_baseline(actual, baseline):
    """校验图片后显式复制，不由失败比较隐式创建。"""
    with Image.open(actual) as image:
        image.verify()
    Path(baseline).parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(actual, baseline)


def main():
    """S3 收尾可使用此命令生成正式基准。"""
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=["update"])
    parser.add_argument("actual", type=Path)
    parser.add_argument("baseline", type=Path)
    args = parser.parse_args()
    update_baseline(args.actual, args.baseline)


if __name__ == "__main__":
    main()
