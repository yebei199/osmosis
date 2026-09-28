# 由 sample.json + essentia.json + clap.json 生成 README 里的 50 首标签表(末三栏留给用户判:E=Essentia 对,C=CLAP 对,两=都对,无=都不对)
import json

# Jamendo 乐器类合并成族再数「乐器数量」;p≥0.2 算有
FAMILY = {
    "guitar": "guitar", "acousticguitar": "guitar", "classicalguitar": "guitar", "electricguitar": "guitar",
    "bass": "bass", "acousticbassguitar": "bass", "doublebass": "bass",
    "drums": "drums", "drummachine": "drums", "beat": "drums", "percussion": "drums", "bongo": "drums",
    "piano": "piano", "electricpiano": "keys", "rhodes": "keys", "keyboard": "keys", "organ": "keys", "pipeorgan": "keys",
    "synthesizer": "synth", "pad": "synth", "sampler": "synth", "computer": "synth",
    "violin": "strings", "viola": "strings", "cello": "strings", "strings": "strings", "orchestra": "strings",
    "voice": "voice",
}
THRESHOLD = 0.2
SIZE_ZH = {"a solo instrument piece": "独奏", "a duet of two instruments": "二重奏",
           "a small ensemble of three or four instruments": "小编制", "a full band": "乐队", "a full orchestra": "管弦乐"}


def families(probs):
    fam = {}
    for c, p in probs.items():
        if p >= THRESHOLD:
            f = FAMILY.get(c, c)
            fam[f] = max(fam.get(f, 0), p)
    return sorted(fam, key=fam.get, reverse=True)


def main():
    e, c = json.load(open("essentia.json")), json.load(open("clap.json"))
    print("| # | 曲目 | Discogs 风格 top3 | Jamendo 风格 top2 | CLAP 风格 top2 | Essentia 乐器(族,p≥0.2) | CLAP 乐器 top3 | 数量:Essentia / CLAP | 风格 | 乐器 | 数量 |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for t in json.load(open("sample.json")):
        er, cr = e[t["id"]], c[t["id"]]
        fam = families(er["instrument_all"])
        row = [
            str(t["n"]),
            f'{t["title"]} — {" / ".join(t["artists"])}'.replace("|", "/"),
            ", ".join(k.split("---")[1] for k in list(er["discogs400"])[:3]),
            ", ".join(list(er["jamendo_genre"])[:2]),
            ", ".join(list(cr["genre"])[:2]),
            ", ".join(fam),
            ", ".join(list(cr["instrument"])[:3]),
            f'{len(fam)} / {SIZE_ZH[next(iter(cr["size"]))]}',
            "", "", "",
        ]
        print("| " + " | ".join(row) + " |")


if __name__ == "__main__":
    main()
