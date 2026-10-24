// AC checks for the headless screenshot tool (MEO-1900). Runs the real thing:
// a headless sway, the app inside pasta, grim. Needs a niri session (to prove
// the user's desktop is untouched), PipeWire, a GPU render node and a built
// target/debug/osmosis-desktop. Run inside `nix-shell slint.nix`.
//
//   nix-shell slint.nix --run 'bun test/headless/check.ts'
//
// Exit 0 only when every check passes; each failure is printed by name.
import { mkdtempSync } from "node:fs";
import { join, resolve } from "node:path";
import { spawnSync } from "bun";
import { workBase } from "./headless.ts";

const root = resolve(import.meta.dir, "../..");
const tool = join(root, "test/headless/headless.ts");
const failures: string[] = [];

function check(name: string, ok: boolean, detail = ""): void {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `  (${detail})` : ""}`);
  if (!ok) failures.push(name);
}

type NiriWindow = { id: number; is_focused: boolean };

function niriWindows(): NiriWindow[] {
  const out = spawnSync(["niri", "msg", "--json", "windows"], { stdout: "pipe" });
  if (out.exitCode !== 0) throw new Error("niri msg failed; this check needs the niri session");
  return JSON.parse(out.stdout.toString());
}

function focusedId(windows: NiriWindow[]): number | undefined {
  return windows.find((w) => w.is_focused)?.id;
}

// Unique colours and the share of non-black pixels in the central band, where
// the Home nebula slot and the card wall sit. A missing GPU texture leaves it flat.
function pixelStats(png: string): { colors: number; lit: number } {
  const crop = ["-gravity", "center", "-crop", "80%x60%+0+0"];
  const colors = spawnSync(["magick", png, ...crop, "-format", "%k", "info:"], { stdout: "pipe" });
  const lit = spawnSync(["magick", png, ...crop, "-colorspace", "gray", "-threshold", "4%", "-format", "%[fx:mean]", "info:"], {
    stdout: "pipe",
  });
  return { colors: Number(colors.stdout.toString()), lit: Number(lit.stdout.toString()) };
}

// Audio links from the given pids to anything but the null sink.
function realAudioLinks(pids: Set<number>): number {
  const dump = spawnSync(["pw-dump"], { stdout: "pipe" });
  type Obj = {
    id: number;
    type: string;
    info?: { props?: Record<string, unknown>; "output-node-id"?: number; "input-node-id"?: number };
  };
  const objs = JSON.parse(dump.stdout.toString()) as Obj[];
  const nodes = new Map(objs.filter((o) => o.type === "PipeWire:Interface:Node").map((o) => [o.id, o.info?.props ?? {}]));
  return objs.filter((o) => {
    if (o.type !== "PipeWire:Interface:Link") return false;
    const from = nodes.get(o.info?.["output-node-id"] ?? -1);
    const to = nodes.get(o.info?.["input-node-id"] ?? -1);
    return pids.has(Number(from?.["application.process.id"])) && to?.["node.name"] !== "osmosis_test_sink";
  }).length;
}

async function waitFor(path: string, ms: number): Promise<boolean> {
  for (let t = 0; t < ms; t += 200) {
    if (await Bun.file(path).exists()) return true;
    await Bun.sleep(200);
  }
  return false;
}

const work = mkdtempSync(join(workBase(), "check-"));
const before = niriWindows();

// AC-5: an AI session pointing the debug build at a desktop-style display is
// refused before any window exists. `wayland-99` does not exist, so even a
// broken guard could not put a window on the user's desktop.
const guarded = spawnSync([join(root, "target/debug/osmosis-desktop")], {
  env: { ...process.env, CLAUDECODE: "1", WAYLAND_DISPLAY: "wayland-99", DISPLAY: "", OSMOSIS_SHOW_WINDOW: "" },
  stdout: "pipe",
  stderr: "pipe",
  timeout: 30_000,
});
check("AC-5 AI session on a desktop display is refused", guarded.exitCode === 2 && guarded.stderr.toString().includes("MEO-1900"), `exit ${guarded.exitCode}`);

// A long-running session stands in for "something else already runs": the
// shot below must start beside it, and it must survive.
const sessionDir = join(work, "session");
const session = Bun.spawn(["bun", tool, "run", "--dir", sessionDir], { stdout: "inherit", stderr: "inherit" });
try {
  const ready = await waitFor(join(sessionDir, "session.json"), 120_000);
  check("session starts", ready);
  if (!ready) throw new Error("session never became ready");
  const info = JSON.parse(await Bun.file(join(sessionDir, "session.json")).text()) as { appPid: number };

  const out = join(work, "shot.png");
  const shot = spawnSync(["bun", tool, "shot", "--out", out, "--width", "1200", "--height", "900"], {
    stdout: "inherit",
    stderr: "inherit",
  });
  check("AC-3 shot succeeds while another instance runs", shot.exitCode === 0, `exit ${shot.exitCode}`);
  check("AC-3 the running instance survives", session.exitCode === null && spawnSync(["kill", "-0", String(info.appPid)]).exitCode === 0);

  const after = niriWindows();
  check("AC-1 focus unchanged", focusedId(before) === focusedId(after), `${focusedId(before)} -> ${focusedId(after)}`);
  const known = new Set(before.map((w) => w.id));
  check("AC-1 no new desktop window", after.every((w) => known.has(w.id)), `${after.length - before.length} new`);
  check("AC-1 no audio link to a real output", realAudioLinks(new Set([info.appPid])) === 0);

  const size = spawnSync(["magick", "identify", "-format", "%wx%h", out], { stdout: "pipe" }).stdout.toString();
  check("shot has the requested size", size === "1200x900", size);
  const stats = pixelStats(out);
  check("AC-2 GPU textures visible (not flat)", stats.colors > 500 && stats.lit > 0.05, `colors=${stats.colors} lit=${stats.lit.toFixed(3)}`);

  const second = join(work, "capture.png");
  const cap = spawnSync(["bun", tool, "capture", "--dir", sessionDir, "--out", second], { stdout: "inherit", stderr: "inherit" });
  check("capture from a running session", cap.exitCode === 0 && (await Bun.file(second).exists()));
} finally {
  session.kill("SIGTERM");
  await session.exited;
}
check("session cleans up on SIGTERM", session.exitCode === 0, `exit ${session.exitCode}`);

console.log(failures.length ? `\n${failures.length} check(s) failed; work dir ${work}` : `\nall checks passed; work dir ${work}`);
process.exit(failures.length ? 1 : 0);
