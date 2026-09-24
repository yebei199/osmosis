#!/usr/bin/env python3
"""从小米 13 的录音里找出两路扫频标记,算两台设备真实出声的偏差。

    analyze.py rec.wav [--dist-l 1.20] [--dist-r 0.03] [--seek-at 120] [--out DIR]

左声道(`L`,1–2.5kHz)与右声道(`R`,4–7kHz)各放在一台设备上,每秒一个标记。
偏差 = L 到达 − R 到达 − 声程差,单位 ms;正数 = 放 L 的那台比放 R 的晚。
`--dist-l/--dist-r` 是两台设备的扬声器到小米麦克风的距离(米),声速按 343m/s。
`--seek-at` 是计划里 seek 的时刻(相对起播,秒),用来单独统计 seek 之后的收敛。

输出:终端一份摘要,`--out` 目录下 `pairs.csv`(每个标记一行)与 `summary.json`。
"""

import argparse
import json
import os
import sys

import numpy as np
from scipy.io import wavfile
from scipy.signal import butter, correlate, find_peaks, hilbert, sosfiltfilt

RATE = 48_000
CHIRP_S = 0.030
BANDS = {"L": (1_000.0, 2_500.0), "R": (4_000.0, 7_000.0)}
SOUND = 343.0
# 标记在每秒的 0.5 秒处(见 src/media.rs)。
PERIOD_S = 1.0


def chirp(f0, f1):
    n = int(CHIRP_S * RATE)
    t = np.arange(n) / RATE
    k = (f1 - f0) / CHIRP_S
    w = 0.5 - 0.5 * np.cos(2 * np.pi * np.arange(n) / (n - 1))
    return np.sin(2 * np.pi * (f0 * t + 0.5 * k * t * t)) * w


def arrivals(x, band):
    """这一频段每个标记的到达时刻(秒,相对录音开头),亚样本精度。"""
    f0, f1 = band
    sos = butter(6, [f0 * 0.8, f1 * 1.15], btype="band", fs=RATE, output="sos")
    y = sosfiltfilt(sos, x)
    ref = chirp(f0, f1)
    c = correlate(y, ref, mode="valid", method="fft")
    env = np.abs(hilbert(c))
    floor = np.median(env)
    thr = max(floor * 12, np.percentile(env, 99.95) * 0.2)
    peaks, _ = find_peaks(env, height=thr, distance=int(0.6 * RATE))
    times = []
    for p in peaks:
        if 0 < p < len(env) - 1:
            a, b, c3 = env[p - 1], env[p], env[p + 1]
            denom = a - 2 * b + c3
            frac = 0.5 * (a - c3) / denom if denom != 0 else 0.0
        else:
            frac = 0.0
        times.append((p + frac) / RATE)
    return np.array(times), float(floor), float(thr)


def pair(tl, tr, window=0.3):
    """L 与 R 按最近邻配对;没有对面的就是缺失(卡顿、没出声)。"""
    pairs = []
    j = 0
    for a in tl:
        while j + 1 < len(tr) and abs(tr[j + 1] - a) <= abs(tr[j] - a):
            j += 1
        if len(tr) and abs(tr[j] - a) <= window:
            pairs.append((a, tr[j]))
    return np.array(pairs)


def stats(v):
    if len(v) == 0:
        return {"n": 0}
    return {
        "n": int(len(v)),
        "median_ms": float(np.median(v)),
        "mean_ms": float(np.mean(v)),
        "std_ms": float(np.std(v)),
        "p5_ms": float(np.percentile(v, 5)),
        "p95_ms": float(np.percentile(v, 95)),
        "max_abs_ms": float(np.max(np.abs(v))),
        "abs_p95_ms": float(np.percentile(np.abs(v), 95)),
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("wav")
    ap.add_argument("--dist-l", type=float, default=0.0)
    ap.add_argument("--dist-r", type=float, default=0.0)
    ap.add_argument("--seek-at", type=float)
    ap.add_argument("--out")
    args = ap.parse_args()

    rate, x = wavfile.read(args.wav)
    if rate != RATE:
        sys.exit(f"录音是 {rate}Hz,要 {RATE}Hz")
    x = x.astype(np.float64)
    if x.ndim > 1:
        x = x[:, 0]
    x /= 32768.0

    tl, floor_l, thr_l = arrivals(x, BANDS["L"])
    tr, floor_r, thr_r = arrivals(x, BANDS["R"])
    pairs = pair(tl, tr)
    acoustic_ms = (args.dist_l - args.dist_r) / SOUND * 1000.0
    if len(pairs):
        offset = (pairs[:, 0] - pairs[:, 1]) * 1000.0 - acoustic_ms
        t0 = pairs[0, 1]
        rel = pairs[:, 1] - t0
    else:
        offset = np.array([])
        rel = np.array([])

    summary = {
        "recording_s": len(x) / RATE,
        "marks_L": int(len(tl)),
        "marks_R": int(len(tr)),
        "pairs": int(len(pairs)),
        "acoustic_correction_ms": acoustic_ms,
        "detect": {"floor_L": floor_l, "thr_L": thr_l, "floor_R": floor_r, "thr_R": thr_r},
    }
    if len(offset):
        # 起播之后前 10 个标记:收敛过程。其余算稳态。
        summary["first10_ms"] = [round(float(v), 3) for v in offset[:10]]
        steady = offset[10:] if len(offset) > 20 else offset
        summary["steady"] = stats(steady)
        # 漂移:稳态偏差对时间做线性拟合,ms/分钟。
        if len(steady) > 20:
            k, _ = np.polyfit(rel[10:] if len(offset) > 20 else rel, steady, 1)
            summary["drift_ms_per_min"] = float(k * 60)
        # 离群:偏离滚动中位数 5ms 以上的标记,当作一次卡顿或跳变。
        med = np.array([np.median(offset[max(0, i - 5): i + 6]) for i in range(len(offset))])
        summary["outliers_over_5ms"] = int(np.sum(np.abs(offset - med) > 5.0))
        # 缺失:相邻标记间隔超过 1.5 个周期的次数。
        summary["gaps_L"] = int(np.sum(np.diff(tl) > 1.5 * PERIOD_S)) if len(tl) > 1 else 0
        summary["gaps_R"] = int(np.sum(np.diff(tr) > 1.5 * PERIOD_S)) if len(tr) > 1 else 0
        if args.seek_at is not None:
            # 第一个标记在起播后 0.5s,所以 seek 在录音相对时间 seek_at − 0.5 处。
            after = rel >= args.seek_at - 0.5
            seg = offset[after]
            summary["after_seek_first10_ms"] = [round(float(v), 3) for v in seg[:10]]
            summary["after_seek"] = stats(seg[10:] if len(seg) > 20 else seg)

    print(json.dumps(summary, ensure_ascii=False, indent=2))
    if args.out:
        os.makedirs(args.out, exist_ok=True)
        with open(os.path.join(args.out, "summary.json"), "w") as f:
            json.dump(summary, f, ensure_ascii=False, indent=2)
        with open(os.path.join(args.out, "pairs.csv"), "w") as f:
            f.write("t_s,L_s,R_s,offset_ms\n")
            for r, (a, b), o in zip(rel, pairs, offset):
                f.write(f"{r:.4f},{a:.6f},{b:.6f},{o:.4f}\n")


if __name__ == "__main__":
    main()
