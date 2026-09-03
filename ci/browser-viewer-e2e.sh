#!/usr/bin/env bash
# The finish line: the SHIPPED viewer, running on the in-tab engine, does the
# real thing — a person opens an invite link in a browser with no daemon and
# the tracker works. Not a wasm-pack probe calling the engine handle: the built
# viewer bundle (app.js + engine.worker.js), the Worker composition root, the
# porthole engine, and the World runner, driven through the actual UI in headless
# Chrome by puppeteer.
#
#   relay + alice's daemon (native, real issues, a reusable invite)
#        ⇑ ws ⇓
#   headless Chrome: the built viewer at #join=<ticket>&relay=<relay>
#        → mints its own seed (OPFS), pulls the Space, renders alice's issues
#
# The viewer mints a FRESH device seed in-tab, so it is a new actor a spent
# single-use invite could not admit — hence the invite here is reusable.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

if ! command -v wasm-pack >/dev/null 2>&1; then
    echo "browser-viewer-e2e: SKIPPED — wasm-pack is not installed." >&2; exit 0
fi
if ! command -v npm >/dev/null 2>&1; then
    echo "browser-viewer-e2e: SKIPPED — npm is not installed." >&2; exit 0
fi
chrome="${CHROME:-}"
if [ -z "$chrome" ]; then
    for c in "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
        "$(command -v google-chrome 2>/dev/null)" "$(command -v chromium 2>/dev/null)"; do
        [ -n "$c" ] && [ -x "$c" ] && { chrome="$c"; break; }
    done
fi
[ -n "$chrome" ] || { echo "browser-viewer-e2e: SKIPPED — no Chrome found." >&2; exit 0; }
export CHROME="$chrome"
wasm_cc="${CC_wasm32_unknown_unknown:-}"
if [ -z "$wasm_cc" ]; then
    for c in /opt/homebrew/opt/llvm@21/bin/clang /opt/homebrew/opt/llvm/bin/clang clang; do
        command -v "$c" >/dev/null 2>&1 && "$c" --print-targets 2>/dev/null | grep -qi wasm && { wasm_cc="$(command -v "$c")"; break; }
    done
fi
[ -n "$wasm_cc" ] || { echo "browser-viewer-e2e: SKIPPED — no wasm clang." >&2; exit 0; }
export CC_wasm32_unknown_unknown="$wasm_cc"
ar_dir="$(dirname "$wasm_cc")"; [ -x "$ar_dir/llvm-ar" ] && export AR_wasm32_unknown_unknown="$ar_dir/llvm-ar"

echo "== build the native half"
cargo build -p lait -p lait-relay -p lait-feed -p world-channel-installer \
    -p lait-issues-runner -p lait-signage-runner --quiet

ROOT="$(mktemp -d)"
export LAIT_DISPLAY=off LAIT_IDLE_SECS=0
unset LAIT_HOME LAIT_STORE LAIT_AGENT LAIT_AS || true
PIDS=(); SERVE_PID=""
cleanup() {
    [ -n "$SERVE_PID" ] && { kill "$SERVE_PID" 2>/dev/null; wait "$SERVE_PID" 2>/dev/null; } || true
    for node in ALICE; do
        t="$(eval "printf '%s' \"\${${node}_TOKEN:-}\"")"; p="$(eval "printf '%s' \"\${${node}_PORT:-}\"")"
        [ -n "$t" ] && [ -n "$p" ] && curl -sS -m 5 -X POST "http://127.0.0.1:${p}/api/host/rpc" \
            -H "Authorization: Bearer ${t}" -H "content-type: application/json" \
            -d '{"cmd":"host_restart"}' >/dev/null 2>&1 || true
    done
    for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; done
    rm -rf "$ROOT"
}
trap cleanup EXIT

port=$(( (RANDOM % 20000) + 40000 ))
relay="http://127.0.0.1:${port}"
echo "== start the relay at ${relay}"
"$root/target/debug/lait-relay" --http "127.0.0.1:${port}" >"$ROOT/relay.log" 2>&1 &
PIDS+=("$!")
curl -sf -o /dev/null --retry 20 --retry-connrefused --retry-delay 1 "$relay" \
    || { echo "::error::relay never came up"; cat "$ROOT/relay.log"; exit 1; }
export LAIT_NETWORK=local LAIT_RELAY="$relay"

if [ -z "${WORLD_FIXTURE_CHANNELS:-}" ]; then
    echo "== publish the signed World fixture channels"
    WORLD_FIXTURE_CHANNELS="$ROOT/channels"
    bash "$root/ci/prepare-independent-world-fixtures.sh" \
        "$WORLD_FIXTURE_CHANNELS" "$root/target/debug" "$root/target/debug/lait-feed"
fi
WORLD_FIXTURE_INSTALLER="${WORLD_FIXTURE_INSTALLER:-$root/target/debug/world-channel-installer}"

start_alice() {
    local ready="$ROOT/alice.ready.json"
    "$WORLD_FIXTURE_INSTALLER" --channels "$WORLD_FIXTURE_CHANNELS" \
        --identity "$ROOT/alice/config" --world com.lait.issues
    LAIT_CONFIG_ROOT="$ROOT/alice/config" "$root/target/debug/lait" --json --port 0 \
        >"$ready" 2>"$ROOT/alice.head.log" &
    PIDS+=("$!")
    for _ in $(seq 1 200); do [ -s "$ready" ] && break; sleep 0.25; done
    [ -s "$ready" ] || { echo "::error::alice never announced:"; cat "$ROOT/alice.head.log"; exit 1; }
    ALICE_TOKEN="$(sed -n 's/.*"token":[[:space:]]*"\{0,1\}\([^",}]*\)"\{0,1\}.*/\1/p' "$ready")"
    ALICE_PORT="$(sed -n 's/.*"port":[[:space:]]*"\{0,1\}\([^",}]*\)"\{0,1\}.*/\1/p' "$ready")"
}
post() { curl -sS --fail-with-body -X POST "http://127.0.0.1:$2$3" \
    -H "Authorization: Bearer $1" -H "content-type: application/json" -d "$4"; }

echo "== alice founds, writes, invites (reusable — the tab is a fresh actor)"
start_alice
post "$ALICE_TOKEN" "$ALICE_PORT" /api/host/rpc \
    "{\"cmd\":\"host_space_found\",\"home\":\"$ROOT/alice/space/.lait\",\"name\":\"Live\",\"nick\":\"alice\"}" >/dev/null
spaces="$(curl -sS --fail-with-body "http://127.0.0.1:${ALICE_PORT}/api/spaces" -H "Authorization: Bearer $ALICE_TOKEN")"
AORB="$(printf '%s' "$spaces" | sed -n 's/.*"id":[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
[ -n "$AORB" ] || { echo "::error::no orbit id"; echo "$spaces"; exit 1; }
post "$ALICE_TOKEN" "$ALICE_PORT" "/api/spaces/$AORB/worlds/issues/rpc" \
    '{"cmd":"project_new","name":"Engineering","key":"ENG"}' >/dev/null
post "$ALICE_TOKEN" "$ALICE_PORT" "/api/spaces/$AORB/worlds/issues/rpc" \
    '{"cmd":"issue_new","title":"the tab pulls this issue","project":"ENG"}' >/dev/null
post "$ALICE_TOKEN" "$ALICE_PORT" "/api/spaces/$AORB/worlds/issues/rpc" \
    '{"cmd":"issue_new","title":"and this one","project":"ENG"}' >/dev/null
invited="$(post "$ALICE_TOKEN" "$ALICE_PORT" "/api/spaces/$AORB/rpc" \
    '{"cmd":"invite","role":"contributor","reusable":true,"ttl_hours":1}')"
# The invite reff is the shareable foundation.pub URL (foundation.pub/i#join=…);
# strip the wrapper back to the bare ticket for a local join against this stack.
REFF="$(printf '%s' "$invited" | sed -n 's/.*"reff":[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
TICKET="${REFF##*join=}"
[ -n "$TICKET" ] || { echo "::error::no ticket"; echo "$invited"; exit 1; }
LINK="lait://join/$TICKET"

echo "== build the engine (porthole) + the runner + the viewer bundle"
( cd "$root/crates/porthole" && wasm-pack build --target web >/dev/null 2>&1 )
engine_wasm="$root/crates/porthole/pkg/porthole_bg.wasm"
[ -f "$engine_wasm" ] || { echo "::error::porthole wasm not built"; exit 1; }
RUSTFLAGS='--cfg getrandom_backend="custom"' cargo build --manifest-path "$root/Cargo.toml" \
    --release --target wasm32-unknown-unknown -p lait-issues-runner --quiet
runner_wasm="$root/target/wasm32-unknown-unknown/release/lait_issues_runner.wasm"
[ -f "$runner_wasm" ] || { echo "::error::runner wasm not built"; exit 1; }
( cd "$root/viewer" && npm run build >/dev/null 2>&1 )
web="$root/products/issues-app/assets/web"
[ -f "$web/index.html" ] && [ -f "$web/engine.worker.js" ] || { echo "::error::viewer bundle missing"; exit 1; }

echo "== serve the shipped bundle + both wasms same-origin"
vport=$(( (RANDOM % 20000) + 20000 ))
node "$root/ci/viewer-e2e/serve.mjs" "$vport" "$web" "$engine_wasm" "$runner_wasm" >"$ROOT/serve.log" 2>&1 &
SERVE_PID=$!
curl -sf -o /dev/null --retry 20 --retry-connrefused --retry-delay 1 "http://127.0.0.1:${vport}/index.html" \
    || { echo "::error::static server never came up"; cat "$ROOT/serve.log"; exit 1; }

echo "== drive the shipped viewer on the join link"
# VIEWER_PKG lets drive.mjs resolve puppeteer-core from viewer/node_modules.
VIEWER_PKG="$root/viewer/package.json" \
    node "$root/ci/viewer-e2e/drive.mjs" "http://127.0.0.1:${vport}" "$LINK" "$relay"
echo "browser-viewer-e2e: the shipped viewer ran the tracker on the in-tab engine."

echo "== drive TWO tabs: live carets + bidirectional convergence on one issue"
# Same stack (relay, alice, reusable invite, served bundle). Two isolated tabs =
# two distinct actors; alice's daemon fans one's caret out to the other.
VIEWER_PKG="$root/viewer/package.json" \
    node "$root/ci/viewer-e2e/drive-two.mjs" \
    "http://127.0.0.1:${vport}" "$LINK" "$relay" "the tab pulls this issue"
echo "browser-viewer-e2e: two shipped-viewer tabs synced text both ways and drew each other's live carets."
