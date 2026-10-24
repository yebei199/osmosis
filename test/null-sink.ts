// Ensure the PipeWire null sink that test and dev instances play into, then
// print its node name for PIPEWIRE_NODE. Idempotent: a second call finds the
// sink the first one left behind (object.linger keeps it until PipeWire
// restarts). Priorities are pinned to 1 so it never becomes the default output.
//
//   PIPEWIRE_NODE=$(bun test/null-sink.ts) target/debug/osmosis-desktop
import { spawnSync } from "bun";

const NAME = "osmosis_test_sink";

function sinkExists(): boolean {
  const dump = spawnSync(["pw-dump"], { stdout: "pipe", stderr: "ignore" });
  if (dump.exitCode !== 0) throw new Error("pw-dump failed; is PipeWire running?");
  const nodes = JSON.parse(dump.stdout.toString()) as { info?: { props?: Record<string, unknown> } }[];
  return nodes.some((n) => n.info?.props?.["node.name"] === NAME);
}

if (!sinkExists()) {
  const props = [
    "factory.name=support.null-audio-sink",
    `node.name=${NAME}`,
    "node.description=osmosis-test-silent",
    "media.class=Audio/Sink",
    "object.linger=true",
    "audio.position=[FL FR]",
    "priority.session=1",
    "priority.driver=1",
  ].join(" ");
  const made = spawnSync(["pw-cli", "create-node", "adapter", `{ ${props} }`], { stdout: "ignore", stderr: "pipe" });
  if (made.exitCode !== 0) throw new Error(`pw-cli create-node failed: ${made.stderr.toString()}`);
  for (let i = 0; i < 20 && !sinkExists(); i++) await Bun.sleep(100);
  if (!sinkExists()) throw new Error(`null sink ${NAME} did not appear`);
}
console.log(NAME);
