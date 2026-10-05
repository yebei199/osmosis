"""按 base..head 的改动从覆盖地图挑场景；认不出就退回全量，宁可多跑不许漏跑。

用法：python selection.py BASE HEAD [--map PATH]，标准输出一份 JSON：
mode 是 subset 或 full；tests 是 run.sh subset 接受的 pytest 节点（full 时为空）；
reasons 逐文件写明选中理由；fallback 写明退回全量的原因。
"""

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

from mutate import FAULTS

SUITE = "test/remote-behavior/"
DEFAULT_MAP = Path(__file__).with_name("coverage-map.json")
# 不起世界、几秒内跑完的检查，子集里总是带上。
ALWAYS = (
    "test_lifecycle.py",
    "test_position.py",
    "test_selection.py",
    "test_remote.py::test_recover_invalid_covers_every_value",
)
# 这些路径不进桌面端与服务端二进制，也不被套件读取，改动不影响任何场景。
IGNORED = re.compile(
    r"""(
        .*\.md
      | LICENSE
      | docs/.*
      | \.(claude|agents|serena)/.*
      | \.(mcp\.json|envrc|gitignore|dockerignore)
      | acceptance/[0-9]+\.toml
      | apps/(android|web|ios)/.*
      | (experiments|xtask|release|docker)/.*
      | Android\.nix
      | server/Dockerfile
      | test/(?!remote-behavior/).*
      | test/remote-behavior/coverage-map\.json
    )""",
    re.VERBOSE,
)
# 不在二进制里的 Rust 测试代码；只由快速层与 status-ui 覆盖。
RUST_TEST = re.compile(r"(.*/)?tests(/.*)?\.rs")
SCENARIO_MODULE = re.compile(re.escape(SUITE) + r"(test_[a-z_]+\.py)")


def git(repo, *args):
    return subprocess.run(
        ["git", *args], cwd=repo, check=True, capture_output=True, text=True
    ).stdout


def changed_files(repo, base, head):
    return set(git(repo, "diff", "--name-only", "--no-renames", base, head).split())


def exists_at(repo, commit, path):
    return (
        subprocess.run(
            ["git", "cat-file", "-e", f"{commit}:{path}"], cwd=repo, capture_output=True
        ).returncode
        == 0
    )


def full(result, reason):
    return result | {"mode": "full", "tests": [], "fallback": reason}


def load_map(repo, head, map_path):
    try:
        data = json.loads(Path(map_path).read_text())
        commit = data["commit"]
        universe = set(data["universe"])
        scenarios = {name: set(files) for name, files in data["scenarios"].items()}
    except FileNotFoundError:
        return None, f"coverage map missing: {map_path}"
    except (ValueError, KeyError, TypeError) as error:
        return None, f"coverage map invalid: {error}"
    ancestor = subprocess.run(
        ["git", "merge-base", "--is-ancestor", commit, head], cwd=repo, capture_output=True
    )
    if ancestor.returncode:
        return None, f"coverage map stale: {commit} is not an ancestor of {head}"
    return (commit, universe, scenarios), None


# 单个文件的判定：返回选中的节点，或 None 表示认不出、须退回全量。
def classify(path, universe, scenarios):
    if path in universe:
        hits = sorted(name for name, files in scenarios.items() if path in files)
        return hits, "covered by these scenarios" if hits else "in binary, executed by no scenario"
    if path.endswith(".rs") and RUST_TEST.fullmatch(path):
        return [], "rust test code, not in the binaries"
    if IGNORED.fullmatch(path):
        return [], "not read by the suite or the binaries"
    if path == SUITE + "mutate.py":
        return [f"test_remote.py::test_fault_sensitivity[{f}]" for f in FAULTS], "mutation driver"
    module = SCENARIO_MODULE.fullmatch(path)
    if module:
        return [module.group(1)], "scenario module"
    return None, "unrecognized file"


def select(repo, base, head, map_path=DEFAULT_MAP):
    repo = Path(repo)
    result = {"base": base, "head": head, "map": str(map_path), "reasons": []}
    loaded, problem = load_map(repo, head, map_path)
    if problem:
        return full(result, problem)
    map_commit, universe, scenarios = loaded
    result["map_commit"] = map_commit
    # 地图之后的改动都让覆盖失真，和本次 base..head 一并计入。
    changed = changed_files(repo, base, head) | changed_files(repo, map_commit, head)
    tests = set(ALWAYS)
    for path in sorted(changed):
        picked, why = classify(path, universe, scenarios)
        if picked is None:
            result["reasons"].append({"file": path, "rule": why})
            return full(result, f"{why}: {path}")
        # ponytail: 显式故障清单，地图里没有 mutate 的嵌套构建。
        faults = [f for f, (anchor, _) in FAULTS.items() if anchor == path]
        picked = picked + [f"test_remote.py::test_fault_sensitivity[{f}]" for f in faults]
        result["reasons"].append({"file": path, "rule": why, "tests": picked})
        tests.update(picked)
    modules = {t for t in tests if "::" not in t}
    tests = {t for t in tests if t.split("::")[0] not in modules or "::" not in t}
    for module in sorted(modules):
        if not exists_at(repo, head, SUITE + module):
            tests.discard(module)
            result["reasons"].append({"file": SUITE + module, "rule": "module removed at head"})
    return result | {"mode": "subset", "tests": sorted(tests), "fallback": None}


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("base")
    parser.add_argument("head")
    parser.add_argument("--map", default=DEFAULT_MAP)
    args = parser.parse_args()
    repo = git(Path(__file__).parent, "rev-parse", "--show-toplevel").strip()
    base, head = (git(repo, "rev-parse", ref).strip() for ref in (args.base, args.head))
    json.dump(select(repo, base, head, args.map), sys.stdout, indent=2)
    print()


if __name__ == "__main__":
    main()
