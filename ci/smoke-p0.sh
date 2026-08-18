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
first_created="$(issues '{"cmd":"issue_new","title":"fix login race","project":"ENG","priority":"high"}')"
second_created="$(issues '{"cmd":"issue_new","title":"add dark mode","project":"ENG","priority":"low"}')"
FIRST_ISSUE="$(printf '%s' "$first_created" | sed -n 's/.*"reff":"\([^"]*\)".*/\1/p')"
SECOND_ISSUE="$(printf '%s' "$second_created" | sed -n 's/.*"reff":"\([^"]*\)".*/\1/p')"
[ -n "$FIRST_ISSUE" ] && [ -n "$SECOND_ISSUE" ] || {
  echo "::error::issue creation did not return canonical refs"
  echo "$first_created"
  echo "$second_created"
  exit 1
}

board="$(issues '{"cmd":"board","project":"ENG","page":{}}')"
has "$board" "fix login race"
has "$board" "add dark mode"

# The work loop: start assigns + activates; done completes (S§5.7).
started="$(issues "{\"cmd\":\"issue_start\",\"reff\":\"$FIRST_ISSUE\"}")"
has "$started" "in_progress"
board="$(issues '{"cmd":"board","project":"ENG","page":{}}')"
has "$board" "in_progress"
issues "{\"cmd\":\"issue_done\",\"reff\":\"$SECOND_ISSUE\"}" >/dev/null
list="$(issues '{"cmd":"list","page":{}}')"
has "$list" "fix login race"

# The activity feed records the transitions.
act="$(issues '{"cmd":"activity","page":{}}')"
has "$act" "created"
has "$act" "started"

# Orientation, on both planes: who this node is, and which build answered.
whoami="$(space '{"cmd":"whoami"}')"
has "$whoami" '"can_write":true'
context="$(host '{"cmd":"host_context"}')"
has "$context" "Smoke"
has "$context" "issues"

# --- the handoff Astrolabe's `Open` performs -------------------------------
# The client mints a launch credential from the head, sends a browser to it, and
# the head exchanges it for a session. Every step of that is here because it is
# the one path in the product where a credential travels in a URL, and the whole
# defence is that the credential is worthless once used.
#
# `-o /dev/null -w %{http_code}` rather than `--fail-with-body`: the assertions
# below are *about* the status codes, including the 401 that has to happen.
code() { curl -sS -o /dev/null -w '%{http_code}' "$@"; }

launch="$(post /api/launch "{\"orbit\":\"${ORBIT}\"}")"
has "$launch" '"ticket"'
has "$launch" "$ORBIT"
TICKET="$(printf '%s' "$launch" | sed -n 's/.*"ticket":"\([^"]*\)".*/\1/p')"
[ -n "$TICKET" ] || { echo "::error::no ticket in:"; echo "$launch"; exit 1; }

# The exchange: a redirect that sets the session cookie and the marker saying a
# client sent this browser. Both, appended rather than replaced — two headers of
# the same name in an array would have the second overwrite the first, and the
# browser would arrive holding the marker with no session behind it.
redeemed="$ROOT/redeemed.txt"
[ "$(code -D "$redeemed" "http://127.0.0.1:${PORT}/?ticket=${TICKET}")" = "303" ] \
  || { echo "::error::a launch link did not redirect:"; cat "$redeemed"; exit 1; }
cookies="$(grep -ci 'set-cookie' "$redeemed" || true)"
[ "$cookies" = "2" ] || { echo "::error::expected a session and a client marker, got $cookies:"; cat "$redeemed"; exit 1; }

# And it is worthless afterwards. A launch URL sits in browser history, in a
# synchronised profile and in the shell's recent list; replay must fail closed.
[ "$(code "http://127.0.0.1:${PORT}/?ticket=${TICKET}")" = "401" ] \
  || { echo "::error::a spent launch link answered a second time"; exit 1; }

# The overlay is client context, and a head somebody opened themselves has none
# to draw. One overlay for a browser the client launched, none for anyone else.
plain="$(curl -sS "http://127.0.0.1:${PORT}/" -H "Authorization: Bearer ${TOKEN}")"
case "$plain" in *data-lait-overlay*) echo "::error::an unlaunched head drew a client overlay"; exit 1 ;; esac
composed="$(curl -sS "http://127.0.0.1:${PORT}/" -H "Cookie: lait_token_${PORT}_client=1; lait_token_${PORT}=${TOKEN}")"
has "$composed" "data-lait-overlay"

# Assets are never composed. A script that differed by how the page was reached
# would make the overlay a thing a World could detect and work around.
a="$(curl -sS "http://127.0.0.1:${PORT}/app.js" -H "Authorization: Bearer ${TOKEN}" | wc -c)"
b="$(curl -sS "http://127.0.0.1:${PORT}/app.js" -H "Cookie: lait_token_${PORT}_client=1; lait_token_${PORT}=${TOKEN}" | wc -c)"
[ "$a" = "$b" ] || { echo "::error::an asset was composed differently for a launched browser ($a vs $b)"; exit 1; }

# Stopping the daemon is a request like any other, and the head survives it —
# which is what makes a self-update take effect.
has "$(host '{"cmd":"host_restart"}')" '"host":"restarting"'

echo "P0 smoke flow OK on ${LABEL}"
