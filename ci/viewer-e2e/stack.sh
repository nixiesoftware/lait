#!/usr/bin/env bash
# Stand up a PERSISTENT viewer-e2e stack (relay + alice + static server) and
# park, so Claude can drive the shipped viewer in Chrome by hand. Reuses the
# already-built artifacts (porthole/pkg, the runner wasm, the viewer bundle);
# writes the join URL + ports to $1, then sleeps. Kill it to tear down.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
info="${1:?usage: stack.sh <infofile>}"

export CC_wasm32_unknown_unknown="${CC_wasm32_unknown_unknown:-/opt/homebrew/opt/llvm@21/bin/clang}"
export AR_wasm32_unknown_unknown="${AR_wasm32_unknown_unknown:-/opt/homebrew/opt/llvm@21/bin/llvm-ar}"

ROOT="$(mktemp -d)"
export LAIT_DISPLAY=off LAIT_IDLE_SECS=0
unset LAIT_HOME LAIT_STORE LAIT_AGENT LAIT_AS || true
cleanup() { pkill -P $$ 2>/dev/null || true; rm -rf "$ROOT"; }
trap cleanup EXIT

port=$(( (RANDOM % 20000) + 40000 ))
relay="http://127.0.0.1:${port}"
"$root/target/debug/lait-relay" --http "127.0.0.1:${port}" >"$ROOT/relay.log" 2>&1 &
curl -sf -o /dev/null --retry 20 --retry-connrefused --retry-delay 1 "$relay" || { echo "relay down"; exit 1; }
export LAIT_NETWORK=local LAIT_RELAY="$relay"

WORLD_FIXTURE_CHANNELS="$ROOT/channels"
bash "$root/ci/prepare-independent-world-fixtures.sh" \
    "$WORLD_FIXTURE_CHANNELS" "$root/target/debug" "$root/target/debug/lait-feed" >/dev/null 2>&1
"$root/target/debug/world-channel-installer" --channels "$WORLD_FIXTURE_CHANNELS" \
    --identity "$ROOT/alice/config" --world com.lait.issues >/dev/null 2>&1
LAIT_CONFIG_ROOT="$ROOT/alice/config" "$root/target/debug/lait" --json --port 0 \
    >"$ROOT/alice.json" 2>"$ROOT/alice.head.log" &
for _ in $(seq 1 200); do [ -s "$ROOT/alice.json" ] && break; sleep 0.25; done
TOKEN="$(sed -n 's/.*"token":[[:space:]]*"\{0,1\}\([^",}]*\)"\{0,1\}.*/\1/p' "$ROOT/alice.json")"
APORT="$(sed -n 's/.*"port":[[:space:]]*"\{0,1\}\([^",}]*\)"\{0,1\}.*/\1/p' "$ROOT/alice.json")"
post() { curl -sS --fail-with-body -X POST "http://127.0.0.1:$APORT$1" \
    -H "Authorization: Bearer $TOKEN" -H "content-type: application/json" -d "$2"; }

post /api/host/rpc "{\"cmd\":\"host_space_found\",\"home\":\"$ROOT/alice/space/.lait\",\"name\":\"Live\",\"nick\":\"alice\"}" >/dev/null
AORB="$(curl -sS "http://127.0.0.1:${APORT}/api/spaces" -H "Authorization: Bearer $TOKEN" | sed -n 's/.*"id":[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
post "/api/spaces/$AORB/worlds/issues/rpc" '{"cmd":"project_new","name":"Engineering","key":"ENG"}' >/dev/null
# An empty body still becomes schema 1 (the router wraps it with the document
# prefix → Typst → ProseMirror LaitDocumentEditor, the collaborative editor),
# and an EMPTY doc mounts editable (a seeded raw body is stored in a non-canonical
# form the editor refuses to edit until Normalized). A bodyless issue would be
# schema 0 (legacy CodeMirror, which writes whole values, no live remote splices).
post "/api/spaces/$AORB/worlds/issues/rpc" '{"cmd":"issue_new","title":"the tab pulls this issue","project":"ENG","body":""}' >/dev/null
post "/api/spaces/$AORB/worlds/issues/rpc" '{"cmd":"issue_new","title":"and this one","project":"ENG"}' >/dev/null
TICKET="$(post "/api/spaces/$AORB/rpc" '{"cmd":"invite","role":"contributor","reusable":true,"ttl_hours":2}' | sed -n 's/.*"reff":[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
LINK="lait://join/$TICKET"

vport=$(( (RANDOM % 20000) + 20000 ))
node "$root/ci/viewer-e2e/serve.mjs" "$vport" "$root/products/issues-app/assets/web" \
    "$root/crates/porthole/pkg/porthole_bg.wasm" \
    "$root/target/wasm32-unknown-unknown/release/lait_issues_runner.wasm" >"$ROOT/serve.log" 2>&1 &
curl -sf -o /dev/null --retry 20 --retry-connrefused --retry-delay 1 "http://127.0.0.1:${vport}/index.html" || { echo "serve down"; exit 1; }

url="http://127.0.0.1:${vport}/#join=$(node -e "process.stdout.write(encodeURIComponent(process.argv[1]))" "$LINK")&relay=$(node -e "process.stdout.write(encodeURIComponent(process.argv[1]))" "$relay")"
{ echo "URL=$url"; echo "RELAY=$relay"; echo "ALICE_PORT=$APORT"; echo "ALICE_TOKEN=$TOKEN"; echo "AORB=$AORB"; echo "VPORT=$vport"; } > "$info"
echo "stack up; info at $info"
sleep 100000
