#!/usr/bin/env bash
# The two-node bench: found, invite, enter, and see each other — on one machine,
# with no infrastructure and nothing leaving loopback.
#
# This is the inner-loop mesh: two scratch identities under disposable config
# roots, `LAIT_NETWORK=isolated` so no relay and no discovery are involved, and
# the invite ticket carrying the host's direct addresses — the exact mechanism a
# LAN/offline Space uses in production. It complements `smoke-p0.sh` (one node
# against its own store): what this proves is the *pair* — founding on one node,
# admission by single-use pass on the other, and membership converging across a
# real wire between two real daemons.
#
# Safe to run on a developer machine with a live identity daemon:
# `LAIT_DISPLAY=off` keeps the scratch daemons off the display coordinator's
# fixed port, config roots are temp dirs, and both heads take ephemeral ports.
#
# Usage: bash ci/bench-two-node.sh

set -euo pipefail

ROOT="$(mktemp -d)"
if command -v cygpath >/dev/null 2>&1; then ROOT="$(cygpath -m "$ROOT")"; fi

# Direct reach only. The ticket carries the host's addresses, so the joiner
# needs no relay and no discovery — and this bench never depends on the
# internet existing.
export LAIT_NETWORK=isolated
# The display coordinator binds one fixed port; a developer machine's real
# daemon may already hold it, and this bench is not about displays.
export LAIT_DISPLAY=off
export LAIT_IDLE_SECS=0
unset LAIT_HOME LAIT_STORE LAIT_AGENT LAIT_AS || true

if command -v cygpath >/dev/null 2>&1; then
  LAIT_BIN="target/debug/lait.exe"
else
  LAIT_BIN="target/debug/lait"
fi
[ -x "$LAIT_BIN" ] || LAIT_BIN="target/debug/lait.exe"
[ -x "$LAIT_BIN" ] || { echo "::error::no built lait binary at target/debug"; exit 1; }
LAIT_BIN="$(cd "$(dirname "$LAIT_BIN")" && pwd)/$(basename "$LAIT_BIN")"

has() { case "$1" in *"$2"*) : ;; *) echo "::error::expected '$2' in:"; echo "$1"; exit 1 ;; esac; }

PIDS=()
cleanup() {
  # The daemons under the heads outlive them by design — closing is not
  # stopping. On a developer machine that would leave two scratch daemons
  # behind per run, so ask each to stop first (`host_restart` stops the daemon
  # once the reply is on the wire; killing the head right after means nothing
  # stands it back up), then take the heads down.
  for node in ALICE BOB; do
    token="$(eval "printf '%s' \"\${${node}_TOKEN:-}\"")"
    port="$(eval "printf '%s' \"\${${node}_PORT:-}\"")"
    [ -n "$token" ] && [ -n "$port" ] && curl -sS -m 5 -X POST \
      "http://127.0.0.1:${port}/api/host/rpc" \
      -H "Authorization: Bearer ${token}" -H "content-type: application/json" \
      -d '{"cmd":"host_restart"}' >/dev/null 2>&1 || true
  done
  for pid in "${PIDS[@]:-}"; do
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  done
}
trap cleanup EXIT

# One head per node, each over its own config root — two whole identities, two
# daemons, two stores. The readiness line lands before the listener accepts, so
# waiting for the line is waiting for the port.
start_node() { # $1 = node name; sets <NAME>_TOKEN / <NAME>_PORT
  local name="$1"
  local ready="$ROOT/$name.ready.json"
  LAIT_CONFIG_ROOT="$ROOT/$name/config" "$LAIT_BIN" --json --port 0 \
    >"$ready" 2>"$ROOT/$name.head.log" &
  PIDS+=("$!")
  for _ in $(seq 1 200); do
    [ -s "$ready" ] && break
    sleep 0.25
  done
  [ -s "$ready" ] || { echo "::error::$name never announced itself:"; cat "$ROOT/$name.head.log"; exit 1; }
  # macOS ships bash 3.2, so no `${name^^}` — uppercase portably.
  local upper; upper="$(printf '%s' "$name" | tr '[:lower:]' '[:upper:]')"
  local field; field() { sed -n "s/.*\"$1\":[[:space:]]*\"\{0,1\}\([^\",}]*\)\"\{0,1\}.*/\1/p" "$ready"; }
  eval "${upper}_TOKEN=\"\$(field token)\""
  eval "${upper}_PORT=\"\$(field port)\""
}

post() { # $1 token, $2 port, $3 path, $4 body
  curl -sS --fail-with-body -X POST "http://127.0.0.1:$2$3" \
    -H "Authorization: Bearer $1" \
    -H "content-type: application/json" \
    -d "$4"
}

start_node alice
start_node bob

# --- alice founds ----------------------------------------------------------
ASTORE="$ROOT/alice/space/.lait"
founded="$(post "$ALICE_TOKEN" "$ALICE_PORT" /api/host/rpc \
  "{\"cmd\":\"host_space_found\",\"home\":\"$ASTORE\",\"name\":\"Bench\",\"nick\":\"alice\"}")"
has "$founded" '"host":"founded"'

spaces="$(curl -sS --fail-with-body "http://127.0.0.1:${ALICE_PORT}/api/spaces" \
  -H "Authorization: Bearer $ALICE_TOKEN")"
AORB="$(printf '%s' "$spaces" | sed -n 's/.*"id":[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
[ -n "$AORB" ] || { echo "::error::no orbit id in:"; echo "$spaces"; exit 1; }

# --- alice invites ---------------------------------------------------------
invited="$(post "$ALICE_TOKEN" "$ALICE_PORT" "/api/spaces/$AORB/rpc" \
  '{"cmd":"invite","role":"contributor","reusable":false,"ttl_hours":1}')"
# The reply's `reff` is the ticket; `lait://join/<ticket>` is the link form the
# host plane's enter accepts (parsed by `runtime::coordinates`).
TICKET="$(printf '%s' "$invited" | sed -n 's/.*"reff":[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
[ -n "$TICKET" ] || { echo "::error::no ticket ref in:"; echo "$invited"; exit 1; }
LINK="lait://join/$TICKET"

# --- bob enters from the ticket -------------------------------------------
BSTORE="$ROOT/bob/space/.lait"
entered="$(post "$BOB_TOKEN" "$BOB_PORT" /api/host/rpc \
  "{\"cmd\":\"host_space_enter\",\"link\":\"$LINK\",\"home\":\"$BSTORE\",\"nick\":\"bob\"}")"
has "$entered" '"host":"entered"'

# --- both sides see the pair ----------------------------------------------
# Admission is the single-use pass; convergence is the wire. The durable fact
# is the second actor with role "member" beside the founding admin — the alias
# ("bob") converges on its own schedule and is deliberately not what this
# asserts on. Poll rather than sleep: a quiet machine passes in a beat, a
# loaded one gets the full budget instead of a flake.
seen=""
for _ in $(seq 1 60); do
  members="$(post "$ALICE_TOKEN" "$ALICE_PORT" "/api/spaces/$AORB/rpc" '{"cmd":"members"}')"
  actors="$(printf '%s' "$members" | grep -o '"key":"act_' | wc -l | tr -d ' ')"
  case "$members" in
    *'"role":"member"'*) [ "$actors" -ge 2 ] && { seen=yes; break; } ;;
  esac
  sleep 1
done
[ -n "$seen" ] || { echo "::error::alice never saw a second member:"; echo "$members"; exit 1; }

echo "bench-two-node: PASS — founded, invited, entered, and converged over an isolated wire"
