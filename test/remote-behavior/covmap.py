"""从一次插桩全量的 profraw 生成覆盖地图，selection.py 读它挑场景。

用法：python covmap.py --profiles DIR --junit JUNIT --object BIN... --out MAP
profiles 下每个 cov-* 目录是一个世界，nodeid 文件写着它所属的 pytest 节点。
"""

import argparse
import datetime
import json
import subprocess
import xml.etree.ElementTree as ET
from pathlib import Path

from selection import ALWAYS


def git(repo, *args):
    return subprocess.run(
        ["git", *args], cwd=repo, check=True, capture_output=True, text=True
    ).stdout.strip()


# 一个世界的全部进程合成一份 profile，按文件列出执行过至少一个函数的源文件，
# 以及每个被插桩函数的首末行；selection.py 只把落在函数体内的改动交给覆盖判断。
def executed(profiles, objects, repo):
    raws = sorted(profiles.glob("*.profraw"))
    if not raws or not any(raw.stat().st_size for raw in raws):
        raise RuntimeError(f"world left no coverage: {profiles}")
    merged = profiles / "merged.profdata"
    subprocess.run(
        ["llvm-profdata", "merge", "-sparse", *map(str, raws), "-o", str(merged)], check=True
    )
    command = ["llvm-cov", "export", "-skip-expansions", "-instr-profile", str(merged)]
    for binary in objects:
        command += ["-object", str(binary)]
    report = json.loads(subprocess.run(command, check=True, capture_output=True).stdout)
    ranges, hits = {}, set()
    for data in report["data"]:
        for function in data["functions"]:
            path = Path(function["filenames"][0])
            # 区域格式：行起、列起、行止、列止、次数、文件号、展开文件号、种类。
            lines = [r for r in function["regions"] if r[5] == 0 and r[7] == 0]
            if not path.is_relative_to(repo) or not lines:
                continue
            name = path.relative_to(repo).as_posix()
            span = (min(r[0] for r in lines), max(r[2] for r in lines))
            ranges.setdefault(name, set()).add(span)
            if function["count"]:
                hits.add(name)
    return ranges, hits


# JUnit 里每个起过世界的用例都必须有覆盖；没起世界的只依赖 Python，按模块挑。
def junit_nodes(junit):
    nodes = set()
    for case in ET.parse(junit).getroot().iter("testcase"):
        nodes.add(f"{case.get('classname', '').split('.')[-1]}.py::{case.get('name')}")
    return nodes


def build(profiles_root, junit, objects, repo):
    repo = Path(repo).resolve()
    functions, scenarios = {}, {}
    for profiles in sorted(Path(profiles_root).glob("cov-*")):
        node = (profiles / "nodeid").read_text().strip()
        ranges, hits = executed(profiles, objects, repo)
        for name, spans in ranges.items():
            functions.setdefault(name, set()).update(spans)
        scenarios[node] = sorted(set(scenarios.get(node, ())) | hits)
    if not scenarios:
        raise RuntimeError("no world recorded coverage")
    nodes = junit_nodes(junit)
    unknown = set(scenarios) - nodes
    if unknown:
        raise RuntimeError(f"coverage for tests absent from JUnit: {sorted(unknown)}")
    unmapped = sorted(
        n for n in nodes - set(scenarios) if n not in ALWAYS and n.split("::")[0] not in ALWAYS
    )
    return {
        "commit": git(repo, "rev-parse", "HEAD"),
        "generated": datetime.datetime.now(datetime.UTC).isoformat(timespec="seconds"),
        # 不起世界的用例：不执行二进制，只在自己的模块或共享设施改动时被选中。
        "unmapped": unmapped,
        # 二进制里每个源文件的被插桩函数首末行，键集合就是地图认得的文件。
        "functions": {
            name: [list(span) for span in sorted(spans)]
            for name, spans in sorted(functions.items())
        },
        "scenarios": dict(sorted(scenarios.items())),
    }


# 每个文件、每个场景各占一行，重新生成后的 git diff 才读得懂。
def dump(data):
    lines = []
    for key, value in data.items():
        if isinstance(value, dict):
            body = ",\n".join(f"  {json.dumps(k)}: {json.dumps(v)}" for k, v in value.items())
            lines.append(f"{json.dumps(key)}: {{\n{body}\n}}")
        else:
            lines.append(f"{json.dumps(key)}: {json.dumps(value)}")
    return "{\n" + ",\n".join(lines) + "\n}\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--profiles", required=True)
    parser.add_argument("--junit", required=True)
    parser.add_argument("--object", action="append", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    repo = git(Path(__file__).parent, "rev-parse", "--show-toplevel")
    data = build(args.profiles, args.junit, args.object, repo)
    Path(args.out).write_text(dump(data))


if __name__ == "__main__":
    main()
