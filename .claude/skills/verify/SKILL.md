---
name: verify
description: Build and drive lait end-to-end on this Windows machine — two-node found/invite/enter flows against the debug binary's HTTP head.
---

# Verifying lait changes against the HTTP head

**There is no CLI.** `lait` is a launcher with three modes — `lait daemon`,
`lait mcp`, and bare `lait [--json] [--port N] [--orbit SEL] [--open]` (the
local app, and the daemon under it). Anything else exits 1. Every operation is a
request to one of three HTTP planes; see `docs/SERVE.md`.

Build: `taskkill //F //IM lait.exe` (a running daemon holds the `.exe` lock),
then `cargo build` → `target/debug/lait.exe`.

## One-node smoke, first

`bash ci/smoke-p0.sh` founds a space, creates a project and issues, walks the
work loop, and reads the activity feed against the real binary. If it fails,
stop there — nothing below will be informative.

## Two-node harness

Two node homes, two heads on two ports. Each head prints one JSON readiness line
(`{url, token, port}`) **before** it accepts, so waiting for the line is waiting
for the port.

```bash
S=/c/Users/.../scratchpad
export LAIT_NETWORK=""            # leave unset for a real wire; `isolated` disables it

LAIT_HOME="$S/alice" target/debug/lait.exe --json --port 7801 > "$S/a.json" &
LAIT_HOME="$S/bob"   target/debug/lait.exe --json --port 7802 > "$S/b.json" &
```

Read `{token, port}` out of each line, then post:

```bash
post() { curl -sS --fail-with-body -X POST "http://127.0.0.1:$2$3" \
  -H "Authorization: Bearer $1" -H 'content-type: application/json' -d "$4"; }

# alice founds
post "$ATOK" 7801 /api/host/rpc '{"cmd":"host_space_found","home":"C:/…/alice/.lait","name":"demo","nick":"alice"}'
AORB=$(curl -sS "http://127.0.0.1:7801/api/spaces" -H "Authorization: Bearer $ATOK" | jq -r '.[0].id')

# alice invites — the reply carries the lait://join/… link
post "$ATOK" 7801 "/api/spaces/$AORB/rpc" '{"cmd":"invite","role":"contributor","reusable":false,"ttl_hours":24}'

# bob enters from it (auto-admitted by the single-use pass)
post "$BTOK" 7802 /api/host/rpc '{"cmd":"host_space_enter","link":"lait://join/…","home":"C:/…/bob/.lait","nick":"bob"}'

# alice sees bob
post "$ATOK" 7801 "/api/spaces/$AORB/rpc" '{"cmd":"who"}'
post "$ATOK" 7801 "/api/spaces/$AORB/rpc" '{"cmd":"members"}'
```

`--fail-with-body` is load-bearing: an engine refusal is a 4xx carrying its own
words, and a bare `curl` under `set -e` exits 0 on it and lets the next
assertion blame the wrong thing.

Paths in JSON are Windows paths — use forward slashes (Windows accepts them and
JSON does not have to escape them), and remember the native binary cannot
resolve a Git-Bash `/tmp/…` path. `cygpath -m` converts.

### The live event surface

`watch --exec` is gone with the CLI. The event stream is SSE:

```bash
curl -sN "http://127.0.0.1:7801/api/events" -H "Authorization: Bearer $ATOK" &
```

Presence, carets, and drained signals ride the `/api/session` WebSocket instead,
which requires an `Origin` header that is one of ours (the upgrade is exempt
from CORS, so an absent Origin is refused there even though ordinary routes
allow it).

To trigger presence transitions, start and stop the other node's head, or send
`{"cmd":"host_restart"}` on its host plane — that stops the daemon under the
head, and the head stands a fresh one back up on the next send.

## Browser checks

`lait --open` opens the app. Do not navigate the SPA with synthetic clicks — see
`CLAUDE.md` for the `lait:nav` event, which is the reliable driver hook.

## Gotchas

- **Kill daemons before building.** A running `lait.exe` holds the link target.
  `taskkill //F //IM lait.exe`, or `Get-Process lait | Stop-Process -Force`.
- **The head binds loopback only** (`127.0.0.1`), never `0.0.0.0`. Nothing off
  this machine can reach it, by design.
- **The token is per run and never persisted.** Restarting a head invalidates
  the old one; the cookie in a stale browser tab is dead. Re-open the printed
  URL — a query token beats a stale cookie, which is exactly why that precedence
  exists.
- **One writer per store.** A second head or daemon on the same `LAIT_HOME`
  contends for the store lock. Point each node at its own home.
- **Nothing is created implicitly.** A home with no store answers a refusal that
  names what does exist. Found or enter first.
- **`--port 0`** takes an ephemeral port when a fixed one may be busy; read the
  real port off the readiness line.
- Scrub ambient `LAIT_HOME`/`LAIT_STORE`/`LAIT_AGENT`/`LAIT_AS` before a run, or
  a live node's state poisons it.
