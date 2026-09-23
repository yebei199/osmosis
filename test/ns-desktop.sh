#!/usr/bin/env bash
# 在自己的 user+net namespace 里(重新)起一个桌面 debug 实例,给 link-loss-e2e.sh 当 `ns:<目录>`。
#
#   test/ns-desktop.sh <目录> [server 端口,缺省 3118] [二进制,缺省 target/debug/osmosis-desktop]
#
# 为什么要 namespace:单实例锁是 abstract socket,按 net ns 隔开 —— 开发机上常驻着一个
# 装机版,不许关它,第二个实例只能换个 ns 起。网络走 slirp4netns,宿主的 127.0.0.1 在 ns
# 里是 10.0.2.2。ns 自己的 127.0.0.1:3000 上起一道 signal-gate,转到宿主那份 server:
# debug 构建烘的地址就是 127.0.0.1:3000,不必重编。
#
# 目录里落下:ns.pid(= 应用进程,exec 之后同一个 pid)、gate-desk.pid、desk.log、
# desk-state/(XDG_STATE_HOME,登录态与设备 id 都在这里,与装机版互不相干)。
# 已有实例就先杀掉再起,返回时 MCP 已经答话。二进制要带 `--features mcp` 且
# SLINT_EMIT_DEBUG_INFO=1 编过(`just desktop-dev` 那一套);本脚本也要在 slint.nix 那个
# nix 环境里跑,二进制的动态库从那里来。第一次起要登录:
# `nsenter -t $(cat <目录>/ns.pid) -U -n --preserve-credentials env PORT=8091 test/mcp-login.sh`。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
D="$(mkdir -p "$1" && cd "$1" && pwd)"
PORT="${2:-3118}"
BIN="${3:-$ROOT/target/debug/osmosis-desktop}"
SLIRP="${SLIRP4NETNS:-$(command -v slirp4netns || true)}"
[ -x "$SLIRP" ] || {
  echo "要 slirp4netns:放进 PATH,或者 SLIRP4NETNS=<路径>" >&2
  exit 1
}

# 内层:ns 里真正跑的那一段。等外层把 slirp 接上再往下走。
if [ "${NS_DESKTOP_INNER:-}" = 1 ]; then
  mount --bind "$D/resolv.conf" /etc/resolv.conf
  ip link set lo up
  echo $$ > "$D/ns.pid"
  while [ ! -e "$D/ns.ready" ]; do sleep 0.2; done
  python3 "$ROOT/test/signal-gate.py" 3000 "$PORT" 10.0.2.2 > "$D/gate-desk.log" 2>&1 &
  echo $! > "$D/gate-desk.pid"
  sleep 0.5
  exec "$BIN"
fi

for f in ns.pid gate-desk.pid; do
  if [ -s "$D/$f" ]; then kill "$(cat "$D/$f")" 2>/dev/null || true; fi
done
sleep 1
rm -f "$D/ns.ready" "$D/ns.pid" "$D/gate-desk.pid" "$D/desk.log"
printf 'nameserver 10.0.2.3\n' > "$D/resolv.conf"
mkdir -p "$D/desk-state"

# 外层:起 ns、接 slirp、等 MCP。整段 setsid 到后台,本脚本返回后实例照跑。
env -u OSMOSIS_API_BASE XDG_STATE_HOME="$D/desk-state" SLINT_MCP_PORT=8091 \
  RUST_LOG="${RUST_LOG:-info}" setsid bash -c '
  NS_DESKTOP_INNER=1 unshare --user --map-root-user --net --mount "$0" "$1" "$2" "$3" &
  child=$!
  for _ in $(seq 50); do [ -s "$1/ns.pid" ] && break; sleep 0.1; done
  "$4" --configure --mtu=65520 "$(cat "$1/ns.pid")" tap0 > "$1/slirp.log" 2>&1 &
  slirp=$!
  sleep 1
  touch "$1/ns.ready"
  wait "$child"
  kill "$slirp" 2>/dev/null || true
' "$0" "$D" "$PORT" "$BIN" "$SLIRP" \
  < /dev/null > "$D/desk.log" 2>&1 &

for _ in $(seq 120); do
  grep -q "MCP server listening" "$D/desk.log" 2>/dev/null && break
  sleep 1
done
grep -q "MCP server listening" "$D/desk.log" || { echo "ns-desktop: $D 没起来,见 $D/desk.log" >&2; exit 1; }
sleep 4
