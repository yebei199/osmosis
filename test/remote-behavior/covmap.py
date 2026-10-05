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


# 一个世界的全部进程合成一份 profile，按文件列出执行过至少一个函数的源文件。
def executed(profiles, objects, repo):
    raws = sorted(profiles.glob("*.profraw"))
    if not raws or not any(raw.stat().st_size for raw in raws):
        raise RuntimeError(f"world left no coverage: {profiles}")
    merged = profiles / "merged.profdata"
    subprocess.run(
        ["llvm-profdata", "merge", "-sparse", *map(str, raws), "-o", str(merged)], check=True
    )
    command = ["llvm-cov", "export", "-summary-only", "-instr-profile", str(merged)]
    for binary in objects:
        command += ["-object", str(binary)]
    report = json.loads(subprocess.run(command, check=True, capture_output=True).stdout)
    universe, hits = set(), set()
    for data in report["data"]:
        for entry in data["files"]:
            path = Path(entry["filename"])
            if not path.is_relative_to(repo):
                continue
            name = path.relative_to(repo).as_posix()
            universe.add(name)
            if entry["summary"]["functions"]["covered"]:
                hits.add(name)
    return universe, hits


# JUnit 里每个起过世界的用例都必须有覆盖；没起世界的只依赖 Python，按模块挑。
def junit_nodes(junit):
    nodes = set()
    for case in ET.parse(junit).getroot().iter("testcase"):
        nodes.add(f"{case.get('classname', '').split('.')[-1]}.py::{case.get('name')}")
    return nodes


def build(profiles_root, junit, objects, repo):
    repo = Path(repo).resolve()
    universe, scenarios = set(), {}
    for profiles in sorted(Path(profiles_root).glob("cov-*")):
        node = (profiles / "nodeid").read_text().strip()
        files, hits = executed(profiles, objects, repo)
        universe |= files
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
        "universe": sorted(universe),
        "scenarios": dict(sorted(scenarios.items())),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--profiles", required=True)
    parser.add_argument("--junit", required=True)
    parser.add_argument("--object", action="append", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    repo = git(Path(__file__).parent, "rev-parse", "--show-toplevel")
    data = build(args.profiles, args.junit, args.object, repo)
    Path(args.out).write_text(json.dumps(data, indent=1) + "\n")


if __name__ == "__main__":
    main()
