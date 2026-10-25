"""变异构建只改变目标组件，复用产物必须可追溯且未经篡改。"""

import hashlib
import json
import subprocess
import xml.etree.ElementTree as ET
from pathlib import Path

import pytest

import mutate
from mutate import third_party_compiles


def test_third_party_compiles_skips_workspace_paths():
    log = """   Compiling ui v0.1.0 (/tmp/fault-x/snapshot/crates/ui)
   Compiling proc-macro2 v1.0.95
   Compiling slint v1.13.0 (https://github.com/yebei199/slint?branch=dev#abc)
    Finished `dev` profile
"""
    assert third_party_compiles(log) == [
        "Compiling proc-macro2 v1.0.95",
        "Compiling slint v1.13.0 (https://github.com/yebei199/slint?branch=dev#abc)",
    ]


@pytest.fixture
def mutation_world(tmp_path, monkeypatch):
    root = tmp_path / "candidate"
    root.mkdir()
    repository = Path(__file__).resolve().parents[2]
    for source, _ in mutate.FAULTS.values():
        path = root / source
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes((repository / source).read_bytes())
    subprocess.run(["git", "init", "-q", str(root)], check=True)
    for name, value in (("user.name", "Fixture"), ("user.email", "fixture@example.invalid")):
        subprocess.run(["git", "-C", str(root), "config", name, value], check=True)
    subprocess.run(["git", "-C", str(root), "add", "."], check=True)
    subprocess.run(["git", "-C", str(root), "commit", "-qm", "fixture"], check=True)
    candidate = subprocess.check_output(
        ["git", "-C", str(root), "rev-parse", "HEAD"], text=True
    ).strip()
    source_target = tmp_path / "completed-build"
    (source_target / "debug").mkdir(parents=True)
    for binary in ("server", "osmosis-desktop"):
        (source_target / "debug" / binary).write_bytes(binary.encode())
    artifacts = tmp_path / "artifacts"
    artifacts.mkdir()
    monkeypatch.setenv("REMOTE_BEHAVIOR_COMMIT", candidate)
    monkeypatch.setenv("REMOTE_BEHAVIOR_TARGET_DIR", str(source_target))
    return root, source_target, artifacts


def install_build_boundary(monkeypatch, tamper=None):
    original = mutate.command
    builds, runs, compiled = [], [], []

    # 仅替换编译器和真实音频世界；git、复制、摘要与身份校验走实际文件系统。
    def command(args, cwd, env, directory, name, timeout):
        if args[0] != "cargo":
            return original(args, cwd, env, directory, name, timeout)
        builds.append(args)
        target = Path(args[args.index("--target-dir") + 1])
        if "--bin" in args:
            binaries = [args[args.index("--bin") + 1]]
        else:
            binaries = [
                "osmosis-desktop" if args[index + 1] == "app-desktop" else "server"
                for index, arg in enumerate(args[:-1])
                if arg == "-p"
            ]
        for binary in binaries:
            compiled.append(binary)
            (target / "debug" / binary).write_bytes(b"rebuilt " + binary.encode())
        (directory / "build.log").write_text("")
        (directory / "build.json").write_text(json.dumps({"command": args, "exit": 0}))
        if tamper in ("source", "copy"):
            base = Path(env["REMOTE_BEHAVIOR_TARGET_DIR"]) if tamper == "source" else target
            (base / "debug/osmosis-desktop").write_bytes(b"tampered")
        return 0

    def run_case(snapshot, target, directory, test):
        runs.append(directory.name)
        case = ET.Element("testcase", name=test)
        if directory.name == "red":
            wanted = "175004" if "reference" in test or "recover" in test else "175001"
            ET.SubElement(
                case, "failure"
            ).text = f"AudioFailure: audio: one did not output {wanted}:"
            if tamper == "after_red":
                (target / "debug/osmosis-desktop").write_bytes(b"tampered")
            return 1, case
        return 0, case

    monkeypatch.setattr(mutate, "command", command)
    monkeypatch.setattr(mutate, "run_case", run_case)
    return builds, runs, compiled


# 四个现有故障仍各跑 RED/GREEN；非目标组件不得随克隆路径重新编译。
@pytest.mark.parametrize("fault", list(mutate.FAULTS))
def test_mutation_reuses_other_component_and_records_identity(mutation_world, monkeypatch, fault):
    root, source_target, artifacts = mutation_world
    builds, runs, compiled = install_build_boundary(monkeypatch)
    mutate.verify(root, artifacts, fault)
    expected = "server" if mutate.FAULTS[fault][0].startswith("server/") else "app-desktop"
    assert compiled == ["server" if expected == "server" else "osmosis-desktop"]
    directory = next(artifacts.iterdir())
    binary = "osmosis-desktop" if expected == "server" else "server"
    source = source_target / "debug" / binary
    record = json.loads((directory / "build.json").read_text())
    assert record["exit"] == 0
    assert record["command"] == builds[0]
    assert record["reused"] == [
        {
        "source": str(source),
        "path": str(directory / "build/debug" / binary),
        "sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
    }]
    assert (directory / "build/debug" / binary).read_bytes() == source.read_bytes()
    assert runs == ["red", "restored"]


# 拷贝、来源在编译中被改写，以及 RED 后被改写，均不能继续进入行为世界。
@pytest.mark.parametrize("tamper", ["source", "copy", "after_red"])
def test_reused_binary_change_refuses_behavior_run(mutation_world, monkeypatch, tamper):
    root, _, artifacts = mutation_world
    _, runs, _ = install_build_boundary(monkeypatch, tamper)
    with pytest.raises(RuntimeError, match="reused binary"):
        mutate.verify(root, artifacts, "protect-reference")
    assert runs == (["red"] if tamper == "after_red" else [])
