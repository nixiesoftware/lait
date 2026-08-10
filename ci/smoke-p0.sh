#!/usr/bin/env bash
# End-to-end smoke against the REAL binary (no network): found a space, create a
# project + issues, render the board, walk the work loop, and read the activity
# feed — asserting on observed output. Proves the daemon + control channel + Loro
# store + HTTP head all work on this OS.
#
# It drives the head a person drives: `lait --json --port 0` prints one readiness
# line ({url, token, port}) and then answers loopback HTTP. There is no command
# surface to type verbs at any more, so the three planes below ARE the product's
# interface — the host plane (`/api/host/rpc`) for founding, the Space plane
# (`/api/spaces/{id}/rpc`) for membership and orientation, and the World plane
# (`/api/spaces/{id}/worlds/issues/rpc`) for the tracker itself.
#
# This runs on every supported OS on every push, and it is the reason the
# platform tier can be a subset rather than the whole suite: a named pipe that
# does not bind, a lock that does not exclude, or a path that NTFS rejects all
# fail here, loudly, in under a minute.
#
# Usage: bash ci/smoke-p0.sh [label]
set -euo pipefail

LABEL="${1:-this platform}"

ROOT="$(mktemp -d)"
# The binary is a native Windows program even when this script runs under Git
# Bash, and it cannot resolve `/tmp/…` — that path lands on whatever drive the
# process happens to be on. `cygpath -m` hands back the Windows form with
# forward slashes, which Windows accepts and JSON does not have to escape.
if command -v cygpath >/dev/null 2>&1; then ROOT="$(cygpath -m "$ROOT")"; fi
export LAIT_CONFIG_ROOT="$ROOT/config"
export LAIT_IDLE_SECS=0
# No wire in this smoke: the whole flow is one node against its own store, and a
# real transport would make the run depend on the runner's network.
export LAIT_NETWORK=isolated
unset LAIT_HOME LAIT_STORE LAIT_AGENT LAIT_AS || true

# Invoke the built binary directly. `cargo run` would re-enter cargo — workspace
# discovery, lockfile check, fingerprinting — for a binary the build step already
# produced. This is still the real binary, which is the point of a smoke test.
if command -v cygpath >/dev/null 2>&1; then
  # A checkout used from both Unix and native Windows can contain both build
  # products. Git Bash must exercise the Windows artifact on the Windows tier.
  LAIT_BIN="target/debug/lait.exe"
else
  LAIT_BIN="target/debug/lait"
fi
[ -x "$LAIT_BIN" ] || LAIT_BIN="target/debug/lait.exe"
[ -x "$LAIT_BIN" ] || { echo "::error::no built lait binary at target/debug"; exit 1; }
LAIT_BIN="$(cd "$(dirname "$LAIT_BIN")" && pwd)/$(basename "$LAIT_BIN")"

has() { case "$1" in *"$2"*) : ;; *) echo "::error::expected '$2' in:"; echo "$1"; exit 1 ;; esac; }

# The build string, with nothing running. The one question support asks first.
version="$("$LAIT_BIN" --version)"
has "$version" "lait "

# --- start the head -------------------------------------------------------
# `--port 0` binds an ephemeral port, so a runner with something on 7777 does
# not fail the smoke for a reason that has nothing to do with lait.
READY="$ROOT/ready.json"
"$LAIT_BIN" --json --port 0 >"$READY" 2>"$ROOT/head.log" &
HEAD_PID=$!

cleanup() {
  kill "$HEAD_PID" 2>/dev/null || true
  wait "$HEAD_PID" 2>/dev/null || true
}
trap cleanup EXIT

# The readiness line is written before the listener accepts, so waiting for the
# line is waiting for the port.
for _ in $(seq 1 200); do
  [ -s "$READY" ] && break
  sleep 0.25
done
[ -s "$READY" ] || { echo "::error::head never announced itself:"; cat "$ROOT/head.log"; exit 1; }

# Read {token, port} out of the JSON with sed rather than jq: this same script
# runs on the Windows runner under Git Bash, and depending on a tool that may or
# may not be on that PATH is a failure mode with nothing to do with lait.
field() { sed -n "s/.*\"$1\":[[:space:]]*\"\{0,1\}\([^\",}]*\)\"\{0,1\}.*/\1/p" "$READY"; }
TOKEN="$(field token)"
PORT="$(field port)"
[ -n "$TOKEN" ] && [ -n "$PORT" ] || { echo "::error::bad readiness line:"; cat "$READY"; exit 1; }

# --- the three planes -----------------------------------------------------
# `--fail-with-body` is load-bearing: an engine refusal is a 4xx carrying its own
# words, and under `set -e` a bare curl would exit 0 on it and let the assertion
# below blame the wrong thing.
post() {
  curl -sS --fail-with-body -X POST "http://127.0.0.1:${PORT}$1" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "content-type: application/json" \
    -d "$2"
}
host() { post /api/host/rpc "$1"; }
space() { post "/api/spaces/${ORBIT}/rpc" "$1"; }
issues() { post "/api/spaces/${ORBIT}/worlds/issues/rpc" "$1"; }

STORE="$ROOT/smoke/.lait"
# Spaces are founded explicitly (nothing is created implicitly); founding seeds a
# project.
founded="$(host "{\"cmd\":\"host_space_found\",\"home\":\"$(printf '%s' "$STORE" | sed 's/\\/\\\\/g')\",\"name\":\"Smoke\",\"nick\":\"smoke\"}")"
has "$founded" '"host":"founded"'

# The catalog is what turns a store path into the id every Space route takes.
spaces_json="$(curl -sS --fail-with-body "http://127.0.0.1:${PORT}/api/spaces" -H "Authorization: Bearer ${TOKEN}")"
has "$spaces_json" "Smoke"
ORBIT="$(printf '%s' "$spaces_json" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
[ -n "$ORBIT" ] || { echo "::error::no orbit id in:"; echo "$spaces_json"; exit 1; }

issues '{"cmd":"project_new","name":"Engineering","key":"ENG"}' >/dev/null
issues '{"cmd":"issue_new","title":"fix login race","project":"ENG","priority":"high"}' >/dev/null
issues '{"cmd":"issue_new","title":"add dark mode","project":"ENG","priority":"low"}' >/dev/null

board="$(issues '{"cmd":"board","project":"ENG"}')"
has "$board" "fix login race"
has "$board" "add dark mode"

# The work loop: start assigns + activates; done completes (S§5.7).
started="$(issues '{"cmd":"issue_start","reff":"ENG-1"}')"
has "$started" "in_progress"
board="$(issues '{"cmd":"board","project":"ENG"}')"
has "$board" "in_progress"
issues '{"cmd":"issue_done","reff":"ENG-2"}' >/dev/null
list="$(issues '{"cmd":"list"}')"
has "$list" "fix login race"

# The activity feed records the transitions.
act="$(issues '{"cmd":"activity"}')"
has "$act" "created"
has "$act" "started"

# Orientation, on both planes: who this node is, and which build answered.
whoami="$(space '{"cmd":"whoami"}')"
has "$whoami" '"can_write":true'
context="$(host '{"cmd":"host_context"}')"
has "$context" "Smoke"
has "$context" "issues"

# Stopping the daemon is a request like any other, and the head survives it —
# which is what makes a self-update take effect.
has "$(host '{"cmd":"host_restart"}')" '"host":"restarting"'

echo "P0 smoke flow OK on ${LABEL}"
