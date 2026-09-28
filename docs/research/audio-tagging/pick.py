# 从「我喜欢」抽 50 首:前 15 位艺人各取最近喜欢的 2 首,其余曲目 seed=162 随机 20 首
import collections
import json
import random
import sys

tracks = json.load(open(sys.argv[1], encoding="utf-8"))["tracks"]
cnt = collections.Counter(t["artists"][0] for t in tracks)
top = [a for a, _ in cnt.most_common(15)]
pick = []
for a in top:
    pick += [t for t in tracks if t["artists"][0] == a][:2]
rest = [t for t in tracks if t["artists"][0] not in top]
pick += random.Random(162).sample(rest, 20)
out = [{"n": i + 1, "id": t["id"], "title": t["title"], "artists": t["artists"],
        "album": t.get("album"), "stratum": "top" if i < 30 else "tail"} for i, t in enumerate(pick)]
json.dump(out, open(sys.argv[2], "w", encoding="utf-8"), ensure_ascii=False, indent=1)
