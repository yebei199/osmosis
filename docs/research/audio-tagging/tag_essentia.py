# Essentia:EffNet-Discogs 嵌入 + 四个分类头,整曲帧平均,每首记耗时
import json, sys, time
import numpy as np
from essentia.standard import MonoLoader, TensorflowPredictEffnetDiscogs, TensorflowPredict2D

M = "models/"
HEADS = {  # name: (file stem, output node)
    "discogs400": "genre_discogs400-discogs-effnet-1",
    "jamendo_instrument": "mtg_jamendo_instrument-discogs-effnet-1",
    "jamendo_genre": "mtg_jamendo_genre-discogs-effnet-1",
    "jamendo_mood": "mtg_jamendo_moodtheme-discogs-effnet-1",
}
emb_model = TensorflowPredictEffnetDiscogs(graphFilename=M + "discogs-effnet-bs64-1.pb", output="PartitionedCall:1")
heads = {}
for k, stem in HEADS.items():
    meta = json.load(open(M + stem + ".json"))
    out = next(o["name"] for o in meta["schema"]["outputs"] if o["output_purpose"] == "predictions")
    inp = meta["schema"]["inputs"][0]["name"]
    heads[k] = (TensorflowPredict2D(graphFilename=M + stem + ".pb", input=inp, output=out), meta["classes"])

res = {}
for t in json.load(open("sample.json")):
    t0 = time.perf_counter()
    audio = MonoLoader(filename="audio/" + t["id"], sampleRate=16000, resampleQuality=4)()
    t1 = time.perf_counter()
    emb = emb_model(audio)
    r = {"dur_s": round(len(audio) / 16000, 1), "load_s": round(t1 - t0, 2)}
    for k, (h, classes) in heads.items():
        p = h(emb).mean(axis=0)
        r[k] = {classes[i]: round(float(p[i]), 3) for i in np.argsort(p)[::-1][:10]}
        if k == "jamendo_instrument":
            r["instrument_all"] = {c: round(float(v), 3) for c, v in zip(classes, p)}
    r["infer_s"] = round(time.perf_counter() - t1, 2)
    res[t["id"]] = r
    print(t["n"], t["title"], r["dur_s"], r["load_s"], r["infer_s"], list(r["discogs400"])[:3], list(r["jamendo_instrument"])[:4], flush=True)
json.dump(res, open(sys.argv[1], "w"), ensure_ascii=False, indent=1)
