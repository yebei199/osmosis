// Run an osmosis desktop instance in an invisible headless display (MEO-1900).
//
// A headless sway composites on a GPU render node, so wgpu textures are real;
// the app runs inside its own pasta network namespace, so the abstract-socket
// single-instance lock does not collide with a dev instance on the user's
// desktop. grim reads the pixels. The user's niri never sees a window.
//
//   bun test/headless/headless.ts shot [--out dist/shot.png] [--width W] [--height H] [--tab T] [--bin B]
//   bun test/headless/headless.ts run [--dir D] [--mcp-port P] [--width W] [--height H] [--tab T] [--bin B] [--log F|-] [--state D]
//   bun test/headless/headless.ts capture --dir D --out F
//
// `run` stays in the foreground (default dir: a fresh one under the cache, `--log -`
// streams the app log to stdout), writes D/session.json once the window is up
// and tears everything down on SIGTERM/SIGINT or when the app exits. Run it
// inside `nix-shell slint.nix`: sway, pasta, grim and the app's libraries come
// from there.
import { accessSync, constants, cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, readlinkSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { spawnSync, type Subprocess } from "bun";

const root = resolve(import.meta.dir, "../..");
const OUTPUT = "HEADLESS-1";
// MCP port inside the namespace; the host side is whatever --mcp-port says.
const NS_MCP_PORT = 8091;
// Debug builds bake 127.0.0.1:3000; pasta maps the namespace's 3000 to the host's.
const BACKEND_PORT = 3000;
const WINDOW_TIMEOUT_MS = 120_000;
// Frames the app gets after its window appears before a shot is taken.
const SETTLE_MS = 3_000;
const DEFAULT_WIDTH = 1440;
const DEFAULT_HEIGHT = 900;

export type Options = {
  dir: string;
  width: number;
  height: number;
  tab: string;
  bin: string;
  mcpPort?: number;
  log?: string;
  // XDG_STATE_HOME used as is; without it the session gets a copy of the dev state.
  state?: string;
  env?: Record<string, string>;
};

export type SessionInfo = { wayland: string; swaysock: string; appPid: number; mcpPort?: number; output: string };

type Session = { info: SessionInfo; sway: Subprocess; pasta: Subprocess };

// Variables that would point the app or its tools at the user's real display,
// at a production backend, or at a proxy that only listens on the host.
const STRIP = [
  "WAYLAND_DISPLAY",
  "DISPLAY",
  "SWAYSOCK",
  "NIRI_SOCKET",
  "I3SOCK",
  "OSMOSIS_API_BASE",
  "HTTPS_PROXY",
  "HTTP_PROXY",
  "ALL_PROXY",
  "https_proxy",
  "http_proxy",
  "all_proxy",
];

function cleanEnv(extra: Record<string, string>): Record<string, string> {
  const env: Record<string, string> = {};
  for (const [k, v] of Object.entries(process.env)) if (v !== undefined && !STRIP.includes(k)) env[k] = v;
  return { ...env, ...extra };
}

// First usable render node; nvidia's does not drive wlroots' gles2 renderer, so
// other vendors go first. OSMOSIS_RENDER_NODE overrides.
export function renderNode(): string {
  if (process.env.OSMOSIS_RENDER_NODE) return process.env.OSMOSIS_RENDER_NODE;
  const usable = readdirSync("/dev/dri")
    .filter((n) => n.startsWith("renderD"))
    .sort()
    .filter((n) => {
      try {
        accessSync(`/dev/dri/${n}`, constants.R_OK | constants.W_OK);
        return true;
      } catch {
        return false;
      }
    });
  const driver = (n: string) => {
    try {
      return basename(readlinkSync(`/sys/class/drm/${n}/device/driver`));
    } catch {
      return "";
    }
  };
  const pick = usable.find((n) => driver(n) !== "nvidia") ?? usable[0];
  if (!pick) throw new Error("no accessible /dev/dri/renderD* node; set OSMOSIS_RENDER_NODE");
  return `/dev/dri/${pick}`;
}

// Session logs outlive the call: the nix shell deletes its own TMPDIR on exit,
// so they go under the user's cache directory instead.
export function workBase(): string {
  const base = join(process.env.XDG_CACHE_HOME ?? join(homedir(), ".cache"), "osmosis-headless");
  mkdirSync(base, { recursive: true });
  return base;
}

function nullSink(): string {
  const r = spawnSync(["bun", join(root, "test/null-sink.ts")], { stdout: "pipe", stderr: "inherit" });
  if (r.exitCode !== 0) throw new Error("test/null-sink.ts failed; refusing to start without a silent sink");
  return r.stdout.toString().trim();
}

// Copy of the dev login state, so the session starts logged in without ever
// writing back into the user's state directory.
function stateCopy(dir: string): string {
  const state = join(dir, "state");
  const dev = join(process.env.XDG_STATE_HOME ?? join(homedir(), ".local/state"), "osmosis-dev");
  mkdirSync(state, { recursive: true });
  if (existsSync(dev) && !existsSync(join(state, "osmosis-dev"))) cpSync(dev, join(state, "osmosis-dev"), { recursive: true });
  return state;
}

async function waitUntil<T>(what: string, probe: () => T | undefined, alive: () => boolean, ms: number): Promise<T> {
  for (let t = 0; t < ms; t += 200) {
    const v = probe();
    if (v !== undefined) return v;
    if (!alive()) throw new Error(`${what}: process exited before it was ready`);
    await Bun.sleep(200);
  }
  throw new Error(`${what}: not ready after ${ms} ms`);
}

type TreeNode = { pid?: number; nodes?: TreeNode[]; floating_nodes?: TreeNode[] };

function windowPid(swaysock: string): number | undefined {
  const r = spawnSync(["swaymsg", "-s", swaysock, "-t", "get_tree", "-r"], { stdout: "pipe", stderr: "ignore" });
  if (r.exitCode !== 0) return undefined;
  const walk = (n: TreeNode): number | undefined => {
    if (n.pid) return n.pid;
    for (const c of [...(n.nodes ?? []), ...(n.floating_nodes ?? [])]) {
      const hit = walk(c);
      if (hit) return hit;
    }
    return undefined;
  };
  return walk(JSON.parse(r.stdout.toString()));
}

function swayConfig(o: Options): string {
  return [
    "xwayland disable",
    `output ${OUTPUT} resolution ${o.width}x${o.height} scale 1`,
    "default_border none",
    "default_floating_border none",
    "",
  ].join("\n");
}

export async function start(o: Options): Promise<Session> {
  if (!existsSync(o.bin)) throw new Error(`${o.bin} missing; build it first (cargo build -p app-desktop)`);
  const runtime = join(o.dir, "run");
  rmSync(runtime, { recursive: true, force: true });
  mkdirSync(runtime, { recursive: true, mode: 0o700 });
  // "-" streams the app log to our own stdout/stderr (desktop-dev wants it live).
  const log = o.log === "-" ? "inherit" : Bun.file(o.log ?? join(o.dir, "desktop.log"));
  const swayLog = join(o.dir, "sway.log");
  const config = join(o.dir, "sway.conf");
  writeFileSync(config, swayConfig(o));

  const sway = Bun.spawn(["sway", "-c", config], {
    env: cleanEnv({
      XDG_RUNTIME_DIR: runtime,
      WLR_BACKENDS: "headless",
      WLR_RENDERER: "gles2",
      WLR_RENDER_DRM_DEVICE: renderNode(),
      WLR_LIBINPUT_NO_DEVICES: "1",
    }),
    stdout: Bun.file(swayLog),
    stderr: Bun.file(swayLog),
  });
  let sockets: { wayland: string; swaysock: string };
  try {
    sockets = await waitUntil(
      "headless sway",
      () => {
        const names = readdirSync(runtime);
        const wl = names.find((n) => /^wayland-\d+$/.test(n));
        const ipc = names.find((n) => n.startsWith("sway-ipc.") && n.endsWith(".sock"));
        return wl && ipc ? { wayland: join(runtime, wl), swaysock: join(runtime, ipc) } : undefined;
      },
      () => sway.exitCode === null,
      20_000,
    );
  } catch (e) {
    sway.kill("SIGKILL");
    throw new Error(`${(e as Error).message}; see ${swayLog}`);
  }

  const tmp = join(o.dir, "tmp");
  mkdirSync(tmp, { recursive: true });
  const forward = o.mcpPort ? `${o.mcpPort}:${NS_MCP_PORT}` : "none";
  const pasta = Bun.spawn(
    ["pasta", "--config-net", "-f", "-q", "--host-lo-to-ns-lo", "-T", String(BACKEND_PORT), "-t", forward, "-u", "none", "-U", "none", "--", o.bin],
    {
      env: cleanEnv({
        XDG_STATE_HOME: o.state ?? stateCopy(o.dir),
        TMPDIR: tmp,
        // OSMOSIS_REAL_AUDIO=1 is the user asking to hear it; anything else stays silent.
        ...(process.env.OSMOSIS_REAL_AUDIO === "1" ? {} : { PIPEWIRE_NODE: nullSink() }),
        OSMOSIS_TAB: o.tab,
        SLINT_MCP_PORT: String(NS_MCP_PORT),
        ...o.env,
        WAYLAND_DISPLAY: sockets.wayland,
      }),
      stdout: log,
      stderr: log,
    },
  );
  const alive = () => sway.exitCode === null && pasta.exitCode === null;
  try {
    const appPid = await waitUntil("app window", () => windowPid(sockets.swaysock), alive, WINDOW_TIMEOUT_MS);
    return { info: { ...sockets, appPid, mcpPort: o.mcpPort, output: OUTPUT }, sway, pasta };
  } catch (e) {
    await stop({ info: { ...sockets, appPid: 0, output: OUTPUT }, sway, pasta });
    throw new Error(`${(e as Error).message}; see the app log and ${swayLog}`);
  }
}

export function capture(info: Pick<SessionInfo, "wayland" | "output">, out: string): void {
  const part = join(dirname(out), `.${basename(out)}.part.png`);
  const r = spawnSync(["grim", "-o", info.output, part], { env: cleanEnv({ WAYLAND_DISPLAY: info.wayland }), stderr: "pipe" });
  if (r.exitCode !== 0) throw new Error(`grim failed: ${r.stderr.toString()}`);
  renameSync(part, out);
}

async function exitsWithin(p: Subprocess, ms: number): Promise<boolean> {
  return Promise.race([p.exited.then(() => true), Bun.sleep(ms).then(() => false)]);
}

export async function stop(s: Session): Promise<void> {
  // End the app itself first; pasta exits once its command has.
  if (s.info.appPid) {
    try {
      process.kill(s.info.appPid, "SIGTERM");
    } catch {}
  }
  for (const p of [s.pasta, s.sway]) {
    if (!(await exitsWithin(p, p === s.pasta ? 5_000 : 0))) p.kill("SIGTERM");
    if (!(await exitsWithin(p, 5_000))) p.kill("SIGKILL");
    await p.exited;
  }
}

function parse(argv: string[]): Record<string, string> {
  const flags: Record<string, string> = {};
  for (let i = 0; i < argv.length; i += 2) {
    if (!argv[i].startsWith("--") || argv[i + 1] === undefined) throw new Error(`bad argument ${argv[i]}`);
    flags[argv[i].slice(2)] = argv[i + 1];
  }
  return flags;
}

function int(v: string | undefined, fallback: number, name: string): number {
  if (v === undefined || v === "") return fallback;
  const n = Number(v);
  if (!Number.isInteger(n) || n <= 0) throw new Error(`--${name} must be a positive integer`);
  return n;
}

function options(f: Record<string, string>, dir: string): Options {
  return {
    dir,
    width: int(f.width, DEFAULT_WIDTH, "width"),
    height: int(f.height, DEFAULT_HEIGHT, "height"),
    tab: f.tab || "0",
    bin: resolve(f.bin ?? join(root, "target/debug/osmosis-desktop")),
    mcpPort: f["mcp-port"] ? int(f["mcp-port"], 0, "mcp-port") : undefined,
    log: f.log && f.log !== "-" ? resolve(f.log) : f.log,
    state: f.state ? resolve(f.state) : undefined,
  };
}

async function shot(f: Record<string, string>): Promise<void> {
  const dir = mkdtempSync(join(workBase(), "shot-"));
  const out = resolve(f.out ?? join(root, "dist/shot.png"));
  mkdirSync(dirname(out), { recursive: true });
  const o = options(f, dir);
  const s = await start(o);
  try {
    await Bun.sleep(SETTLE_MS);
    capture(s.info, out);
  } finally {
    await stop(s);
  }
  console.log(`${out}  ← headless ${o.width}x${o.height} logical px (logs in ${dir})`);
}

async function run(f: Record<string, string>): Promise<never> {
  const dir = f.dir ? resolve(f.dir) : mkdtempSync(join(workBase(), "run-"));
  mkdirSync(dir, { recursive: true });
  const s = await start(options(f, dir));
  let stopping: Promise<void> | undefined;
  const shutdown = () => (stopping ??= stop(s));
  for (const sig of ["SIGTERM", "SIGINT", "SIGHUP"] as const) process.on(sig, () => void shutdown().then(() => process.exit(0)));
  writeFileSync(join(dir, "session.json"), JSON.stringify(s.info));
  console.log(`headless session ready: ${join(dir, "session.json")}${s.info.mcpPort ? `, MCP on 127.0.0.1:${s.info.mcpPort}` : ""}`);
  const code = await s.pasta.exited;
  await shutdown();
  process.exit(code === 0 ? 0 : 1);
}

async function main(): Promise<void> {
  const [cmd, ...rest] = process.argv.slice(2);
  const f = parse(rest);
  if (cmd === "shot") return shot(f);
  if (cmd === "run") return run(f);
  if (cmd === "capture") {
    if (!f.dir || !f.out) throw new Error("capture needs --dir and --out");
    return capture(JSON.parse(readFileSync(join(resolve(f.dir), "session.json"), "utf8")), resolve(f.out));
  }
  console.error("usage: headless.ts shot|run|capture [--flags]; see the header of this file");
  process.exit(2);
}

if (import.meta.main) {
  main().catch((e) => {
    console.error(`headless: ${(e as Error).message}`);
    process.exit(1);
  });
}
