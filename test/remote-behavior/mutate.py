"""在固定临时快照中施加单一故障，以真实 pytest 音频失败确认灵敏度。"""

import json
import os
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path

from lifecycle import run_logged

FAULTS = {
    "send-request": ("crates/ui/src/sync/group.rs", "test_core_pick[list]"),
    "execute-target": ("crates/ui/src/music/playback/group.rs", "test_core_pick[list]"),
    "recover-reference": (
        "server/src/syncplay/group.rs",
        "test_recover_invalid[revision-direct-playing]",
    ),
    "protect-reference": (
        "server/src/store/queue.rs",
        "test_radio_publication_preserves_group_reference",
    ),
}


# 仅替换固定且唯一的源码片段；上游漂移使注入失败，不猜测位置。
def change(snapshot, fault):
    path = snapshot / FAULTS[fault][0]
    source = path.read_text()
    if fault == "send-request":
        before = "api::group_play(&me, pick).await"
        after = "{ let _ = pick; Ok(group.state()) }"
    elif fault == "execute-target":
        before = "    play_current(ui, deck);"
        after = "    let _ = (ui, deck);"
    elif fault == "recover-reference":
        start = source.index("    group.now = None;", source.index("async fn current_entries("))
        end = source.index("\n}", start)
        before = source[start:end]
        after = "    Err(AppError::NotFound)"
    else:
        before = """           AND NOT EXISTS (
                 SELECT 1 FROM play_groups g
                 WHERE g.queue_id = e.queue_id AND g.revision = e.revision)"""
        after = ""
    if source.count(before) != 1:
        raise RuntimeError(f"mutation anchor is not unique: {fault}")
    path.write_text(source.replace(before, after, 1))


# 每条命令记录时间、完整日志和真实退出码，超时不算行为 RED。
def command(args, cwd, env, directory, name, timeout):
    return run_logged(args, cwd, env, directory, name, timeout)


# 定向套件仍运行真实世界；限制为一个 testcase，避免无关失败污染证明。
def run_case(snapshot, target, directory, test):
    directory.mkdir()
    env = os.environ | {
        "REMOTE_BEHAVIOR_TARGET_DIR": str(target),
        "REMOTE_BEHAVIOR_ARTIFACTS": str(directory),
    }
    xml = directory / "junit.xml"
    status = command(
        [
            sys.executable,
            "-m",
            "pytest",
            "--rootdir",
            str(snapshot / "test/remote-behavior"),
            str(snapshot / "test/remote-behavior/test_remote.py") + "::" + test,
            "--junitxml",
            str(xml),
            "-o",
            "addopts=--strict-config --strict-markers",
        ],
        snapshot,
        env,
        directory,
        "pytest",
        1200,
    )
    if not xml.exists():
        raise RuntimeError(f"nested pytest produced no JUnit: exit {status}")
    cases = ET.parse(xml).getroot().findall(".//testcase")
    if len(cases) != 1 or cases[0].get("name") != test:
        raise RuntimeError("nested JUnit is not the exact requested testcase")
    return status, cases[0]


# RED 必须是最终音频断言；恢复候选在同一个精确 testcase 上必须 GREEN。
def verify(root: Path, artifacts: Path, fault: str):
    directory = Path(tempfile.mkdtemp(prefix="fault-", dir=artifacts))
    snapshot = directory / "snapshot"
    candidate = os.environ["REMOTE_BEHAVIOR_COMMIT"]
    env = dict(os.environ)
    if command(
        ["git", "clone", "--shared", "--no-checkout", str(root), str(snapshot)],
        root,
        env,
        directory,
        "clone",
        60,
    ):
        raise RuntimeError("cannot create independent snapshot")
    if command(
        ["git", "checkout", "--detach", candidate], snapshot, env, directory, "checkout", 60
    ):
        raise RuntimeError("cannot pin mutation snapshot")
    change(snapshot, fault)
    patch = subprocess.check_output(["git", "diff", "--", FAULTS[fault][0]], cwd=snapshot)
    (directory / "mutation.patch").write_bytes(patch)
    target = directory / "build"
    env.pop("OSMOSIS_API_BASE", None)
    env["SLINT_EMIT_DEBUG_INFO"] = "1"
    build = [
        "cargo",
        "build",
        "--locked",
        "--config",
        'profile.dev.package."*".opt-level=0',
        "-p",
        "app-desktop",
        "-p",
        "server",
        "--features",
        "app-desktop/mcp",
        "--target-dir",
        str(target),
    ]
    if command(build, snapshot, env, directory, "build", 3600):
        raise RuntimeError("mutation did not compile; this is not a valid RED")
    test = FAULTS[fault][1]
    status, case = run_case(snapshot, target, directory / "red", test)
    failure = case.find("failure")
    wanted = "175004" if fault in ("recover-reference", "protect-reference") else "175001"
    failure_text = "" if failure is None else (failure.text or "")
    if (
        status != 1
        or failure is None
        or case.find("error") is not None
        or case.find("skipped") is not None
        or "AudioFailure" not in failure_text
        or f"audio: one did not output {wanted}:" not in failure_text
    ):
        raise AssertionError("mutation must fail a final PCM assertion, not setup/compilation/skip")
    green_status, green = run_case(
        root, Path(os.environ["REMOTE_BEHAVIOR_TARGET_DIR"]), directory / "restored", test
    )
    if green_status or any(green.find(tag) is not None for tag in ("failure", "error", "skipped")):
        raise AssertionError("restored candidate did not pass the same behavior testcase")
    (directory / "sensitivity.json").write_text(
        json.dumps(
            {
                "candidate": candidate,
                "fault": fault,
                "test": test,
                "red": status,
                "restored": green_status,
                "patch": str(directory / "mutation.patch"),
            },
            indent=2,
        )
    )
