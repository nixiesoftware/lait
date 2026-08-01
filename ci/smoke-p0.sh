#!/usr/bin/env bash
# End-to-end smoke against the REAL binary (no network): drive the P0 tracker
# flow — create a project + issues, render the board, edit an issue, and read
# the activity feed — asserting on observed output. Proves the daemon + control
# channel + Loro store + CLI all work on this OS.
#
# This runs on every supported OS on every push. It is the one test that
# exercises the whole stack through the same surface a user does, and it is the
# reason the platform tier can be a subset rather than the whole suite: a
# named pipe that does not bind, a lock that does not exclude, or a path that
# NTFS rejects all fail here, loudly, in under a minute.
#
# Usage: bash ci/smoke-p0.sh [label]
set -euo pipefail

LABEL="${1:-this platform}"

export LAIT_HOME="$(mktemp -d)"
export LAIT_IDLE_SECS=0

# Capture output into a variable rather than piping into `grep -q`: an
# early-closing reader (grep -q, head) would send SIGPIPE to the CLI — which
# resets SIGPIPE to default for clean interactive piping — and under `pipefail`
# that races the smoke. Capturing drains stdout fully, so the assertions are
# deterministic regardless of buffering/timing.
#
# Invoke the built binary directly. `cargo run` would re-enter cargo for every
# one of these ~15 calls — workspace discovery, lockfile check, and
# fingerprinting each time — for a binary the build step already produced. This
# is still the real binary, which is the point of a smoke test; it just stops
# paying cargo's startup per command.
LAIT_BIN="target/debug/lait"
[ -x "$LAIT_BIN" ] || LAIT_BIN="target/debug/lait.exe"
[ -x "$LAIT_BIN" ] || { echo "::error::no built lait binary at target/debug"; exit 1; }
LAIT_BIN="$(cd "$(dirname "$LAIT_BIN")" && pwd)/$(basename "$LAIT_BIN")"

bin() { "$LAIT_BIN" "$@"; }
has() { case "$1" in *"$2"*) : ;; *) echo "::error::expected '$2' in:"; echo "$1"; exit 1 ;; esac; }

# id is 64 lowercase hex chars
id_out="$(bin id)"
[[ "$id_out" =~ ^[0-9a-f]{64}$ ]] || { echo "::error::bad id: $id_out"; exit 1; }
# spaces are founded explicitly (no lazy mint); founding seeds a project
bin init --name Smoke
# Product verbs live under their World's namespace (`lait issues ...`); the
# root shell navigates identities, Orbits, Spaces and Worlds.
bin issues projects add ENG Engineering
bin issues new "fix login race" -p ENG -P high >/dev/null
bin issues new "add dark mode"  -p ENG -P low  >/dev/null
# board shows both issues in Backlog
board="$(bin issues board ENG)"; has "$board" "fix login race"; has "$board" "add dark mode"
# the work loop: start assigns + activates; done completes (S§5.7)
start_out="$(bin issues start ENG-1 --no-branch)"; has "$start_out" "in_progress"
board="$(bin issues board ENG)"; has "$board" "In Progress"; has "$board" "fix login race"
bin issues done ENG-2 >/dev/null
ls_out="$(bin issues ls)"; has "$ls_out" "fix login race"
# activity feed records the transitions
act="$(bin issues activity)"; has "$act" "created"; has "$act" "started"
# the machine registry lists the founded space
orbits_out="$(bin orbits)"; has "$orbits_out" "Smoke"
bin shutdown || true
echo "P0 smoke flow OK on ${LABEL}"
