# CLAP 零样本:每首取 25%/50%/75% 处三段 10s,音频嵌入平均后与文本提示比余弦相似度
import json, sys, time
import numpy as np, torch, laion_clap
from essentia.standard import MonoLoader

dev = sys.argv[2] if len(sys.argv) > 2 else "cuda"
GENRES = ["neoclassical piano", "anime soundtrack", "epic orchestral film score", "j-rock", "j-pop",
          "anime song", "lo-fi hip hop", "hip hop", "phonk", "post-rock", "ambient", "electronic dance music",
          "chinese traditional music", "folk", "classical string quartet", "acoustic singer-songwriter",
          "r&b", "trap", "cinematic", "pop"]
INSTR = ["piano", "acoustic guitar", "electric guitar", "bass guitar", "drums", "violin", "cello",
         "string ensemble", "synthesizer", "vocals", "choir", "flute", "erhu", "guzheng", "brass", "harp",
         "electronic beats", "orchestra"]
SIZE = ["a solo instrument piece", "a duet of two instruments", "a small ensemble of three or four instruments",
        "a full band", "a full orchestra"]
model = laion_clap.CLAP_Module(enable_fusion=False, amodel="HTSAT-base", device=dev)
model.load_ckpt("models/music_audioset_epoch_15_esc_90.14.pt", verbose=False)

def text_emb(labels, tmpl):
    with torch.no_grad():
        e = model.get_text_embedding([tmpl.format(l) for l in labels], use_tensor=True)
    return torch.nn.functional.normalize(e, dim=-1)

T = {"genre": (GENRES, text_emb(GENRES, "This is a {} track.")),
     "instrument": (INSTR, text_emb(INSTR, "This is a music of {}.")),
     "size": (SIZE, text_emb(SIZE, "This is {}."))}
res = {}
for t in json.load(open("sample.json")):
    t0 = time.perf_counter()
    a = MonoLoader(filename="audio/" + t["id"], sampleRate=48000, resampleQuality=4)()
    t1 = time.perf_counter()
    L = 480000
    segs = np.stack([np.pad(a[int(len(a) * f) - L // 2:][:L], (0, max(0, L - len(a[int(len(a) * f) - L // 2:][:L])))) for f in (0.25, 0.5, 0.75)])
    with torch.no_grad():
        ae = model.get_audio_embedding_from_data(x=torch.from_numpy(segs).float().to(dev), use_tensor=True)
        ae = torch.nn.functional.normalize(ae.mean(0, keepdim=True), dim=-1)
    r = {"load_s": round(t1 - t0, 2)}
    for k, (labels, te) in T.items():
        s = (ae @ te.T).squeeze(0).cpu().numpy()
        r[k] = {labels[i]: round(float(s[i]), 3) for i in np.argsort(s)[::-1]}
    if dev == "cuda": torch.cuda.synchronize()
    r["infer_s"] = round(time.perf_counter() - t1, 3)
    res[t["id"]] = r
    print(t["n"], t["title"], r["load_s"], r["infer_s"], list(r["genre"])[:3], list(r["instrument"])[:4], list(r["size"])[0], flush=True)
json.dump(res, open(sys.argv[1], "w"), ensure_ascii=False, indent=1)
