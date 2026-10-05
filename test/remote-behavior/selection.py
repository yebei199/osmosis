"""按 base..head 的改动从覆盖地图挑场景；认不出就退回全量，宁可多跑不许漏跑。

用法：python selection.py BASE HEAD [--out FILE] [--args]，标准输出 accept-select/1 JSON
（acceptance/run.toml 的 [select]，格式见 nixos_config#289）：mode 是 subset 或 full；
tests 是 acceptance/run.sh subset 接受的 pytest 节点（full 时为空）；fallback 写明退回
全量的原因；reasons 给每个改动文件一条，带 tests 或 "full": true。
输出只取决于两个提交：地图从 head 的提交里读，不读工作区，不带路径与时间。
--args 另外逐行打印 run.sh 的参数（all，或 subset 加节点），acceptance/run.sh changed 用它。
"""

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

from mutate import FAULTS

SUITE = "test/remote-behavior/"
MAP = SUITE + "coverage-map.json"
SCHEMA = "accept-select/1"
# 不起世界、几秒内跑完的检查，子集里总是带上。
ALWAYS = (
    "test_lifecycle.py",
    "test_position.py",
    "test_selection.py",
    "test_covmap.py",
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
HUNK = re.compile(r"@@ -(\d+)(?:,(\d+))? ")
# 只有注释或空白的改动不改变行为。
INERT = re.compile(r"\s*(//.*)?")


def git(repo, *args):
    return subprocess.run(
        ["git", *args], cwd=repo, check=True, capture_output=True, text=True
    ).stdout


def changed_files(repo, base, head):
    return set(git(repo, "diff", "--name-only", "--no-renames", base, head).split())


def exists_at(repo, commit, path):
    return (
        subprocess.run(
            ["git", "cat-file", "-e", f"{commit}:{path}"],
            cwd=repo,
            capture_output=True,
            check=False,
        ).returncode
        == 0
    )


def output(mode, tests, fallback, reasons):
    return {
        "schema": SCHEMA,
        "mode": mode,
        "tests": tests,
        "fallback": fallback,
        "reasons": reasons,
    }


def load_map(repo, head):
    shown = subprocess.run(
        ["git", "show", f"{head}:{MAP}"], cwd=repo, capture_output=True, text=True, check=False
    )
    if shown.returncode:
        return None, f"coverage map missing at head: {MAP}"
    try:
        data = json.loads(shown.stdout)
        commit = data["commit"]
        functions = {path: [tuple(r) for r in ranges] for path, ranges in data["functions"].items()}
        scenarios = {name: set(files) for name, files in data["scenarios"].items()}
    except (ValueError, KeyError, TypeError) as error:
        return None, f"coverage map invalid: {error}"
    ancestor = subprocess.run(
        ["git", "merge-base", "--is-ancestor", commit, head],
        cwd=repo,
        capture_output=True,
        check=False,
    )
    if ancestor.returncode:
        return None, f"coverage map stale: {commit} is not an ancestor of {head}"
    return (commit, functions, scenarios), None


# 覆盖只认函数体：derive 展开、常量、类型和 impl 头都不计执行，改到它们就认不出影响面。
# 每段改动（地图提交的行号）都须落在某个被插桩函数的首末行之间，且不碰首末行。
def inside_bodies(repo, map_commit, head, path, ranges):
    if not exists_at(repo, head, path):
        return False
    diff = git(repo, "diff", "-U0", "--no-renames", map_commit, head, "--", path)
    hunks = []
    for line in diff.splitlines():
        match = HUNK.match(line)
        if match:
            start, count = int(match.group(1)), int(match.group(2) or 1)
            hunks.append((start, count, []))
        elif hunks and line[:1] in "+-" and not line.startswith(("+++", "---")):
            hunks[-1][2].append(line[1:])
    for start, count, text in hunks:
        if all(INERT.fullmatch(t) for t in text):
            continue
        last = start if count == 0 else start + count - 1
        lower = start if count == 0 else start - 1
        if not any(first <= lower and last < end for first, end in ranges):
            return False
    return True


# 单个文件的判定：返回选中的节点，或 None 表示认不出、须退回全量。
def classify(path, functions, scenarios, body_only, present):
    if path in functions:
        if not body_only(path):
            return None, "change outside function bodies (types, derives, consts, signatures)"
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
        if not present(path):
            return [], "scenario module removed at head"
        return [module.group(1)], "scenario module"
    return None, "unrecognized file"


def select(repo, base, head):
    repo = Path(repo)
    loaded, problem = load_map(repo, head)
    if problem:
        # 门禁核对每个改动文件都有判定，地图坏了也逐个标成认不出。
        reasons = [
            {"file": path, "rule": problem, "full": True}
            for path in sorted(changed_files(repo, base, head))
        ]
        return output("full", [], problem, reasons)
    map_commit, functions, scenarios = loaded
    # 地图之后的改动都让覆盖失真，和本次 base..head 一并计入。
    changed = changed_files(repo, base, head) | changed_files(repo, map_commit, head)
    tests, reasons = set(ALWAYS), []
    for path in sorted(changed):
        picked, why = classify(
            path,
            functions,
            scenarios,
            lambda p: inside_bodies(repo, map_commit, head, p, functions[p]),
            lambda p: exists_at(repo, head, p),
        )
        if picked is None:
            # 不提前返回：报告要列出每个认不出的文件。
            reasons.append({"file": path, "rule": why, "full": True})
            continue
        # ponytail: 显式故障清单，地图里没有 mutate 的嵌套构建。
        faults = [f for f, (anchor, _) in FAULTS.items() if anchor == path]
        picked = picked + [f"test_remote.py::test_fault_sensitivity[{f}]" for f in faults]
        reasons.append({"file": path, "rule": why, "tests": picked})
        tests.update(picked)
    unknown = [r["file"] for r in reasons if r.get("full")]
    if unknown:
        return output("full", [], f"{len(unknown)} unrecognized: " + ", ".join(unknown), reasons)
    modules = {t for t in tests if "::" not in t}
    tests = {t for t in tests if t.split("::")[0] not in modules or "::" not in t}
    return output("subset", sorted(tests), None, reasons)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("base")
    parser.add_argument("head")
    parser.add_argument("--out", help="write the JSON here instead of stdout")
    parser.add_argument("--args", action="store_true", help="print run.sh arguments, one per line")
    args = parser.parse_args()
    repo = git(Path(__file__).parent, "rev-parse", "--show-toplevel").strip()
    base, head = (git(repo, "rev-parse", ref).strip() for ref in (args.base, args.head))
    result = select(repo, base, head)
    text = json.dumps(result, indent=2) + "\n"
    if args.out:
        Path(args.out).write_text(text)
    else:
        sys.stdout.write(text)
    if args.args:
        print("\n".join(["all"] if result["mode"] == "full" else ["subset", *result["tests"]]))


if __name__ == "__main__":
    main()
