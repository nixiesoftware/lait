#!/usr/bin/env bash
# The live-network claim, end to end: a wasm engine in headless Chrome
# reaches a native peer through lait's own relay — all three processes local,
# nothing mocked, no public infrastructure.
#
#   lait-relay (plain HTTP)  ⇐ ws ⇒  browser worker (comms/iroh, wasm32)
#            ⇑ ws ⇓
#   comms/examples/live_echo (native, LAIT_NETWORK=local)
#
# wasm tests carry no runtime environment, so the rendezvous (relay URL, the
# peer's device id) is baked in at compile time; the test source is touched
# first so a stale build can never carry yesterday's rendezvous.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

# Skip-honesty: a skip is "could not be asked", never a pass.
if ! command -v wasm-pack >/dev/null 2>&1; then
    echo "browser-live: SKIPPED — wasm-pack is not installed." >&2
    exit 0
fi
if [ -z "${CHROME:-}" ] \
    && ! command -v google-chrome >/dev/null 2>&1 \
    && ! command -v chromium >/dev/null 2>&1 \
    && [ ! -e "/Applications/Google Chrome.app" ]; then
    echo "browser-live: SKIPPED — no Chrome found." >&2
    exit 0
fi
wasm_cc="${CC_wasm32_unknown_unknown:-}"
if [ -z "$wasm_cc" ]; then
    for candidate in /opt/homebrew/opt/llvm@21/bin/clang /opt/homebrew/opt/llvm/bin/clang \
        /usr/bin/clang clang; do
        if command -v "$candidate" >/dev/null 2>&1 \
            && "$candidate" --print-targets 2>/dev/null | grep -qi wasm; then
            wasm_cc="$(command -v "$candidate")"
            break
        fi
    done
fi
if [ -z "$wasm_cc" ]; then
    echo "browser-live: SKIPPED — no clang with a wasm backend (ring needs one)." >&2
    exit 0
fi
export CC_wasm32_unknown_unknown="$wasm_cc"
ar_dir="$(dirname "$wasm_cc")"
[ -x "$ar_dir/llvm-ar" ] && export AR_wasm32_unknown_unknown="$ar_dir/llvm-ar"

echo "== build the native half"
cargo build -p lait-relay --quiet
cargo build -p comms --example live_echo --quiet

scratch="$(mktemp -d)"
relay_pid=""
peer_pid=""
cleanup() {
    [ -n "$peer_pid" ] && { kill "$peer_pid" 2>/dev/null; wait "$peer_pid" 2>/dev/null; } || true
    [ -n "$relay_pid" ] && { kill "$relay_pid" 2>/dev/null; wait "$relay_pid" 2>/dev/null; } || true
    rm -rf "$scratch"
}
trap cleanup EXIT

# The relay advertises its bind string verbatim, so the port is chosen here —
# binding :0 would advertise :0.
port=$(( (RANDOM % 20000) + 40000 ))
relay="http://127.0.0.1:${port}"

echo "== start the relay at ${relay}"
"$root/target/debug/lait-relay" --http "127.0.0.1:${port}" >"$scratch/relay.log" 2>&1 &
relay_pid=$!
curl -sf -o /dev/null --retry 20 --retry-connrefused --retry-delay 1 "$relay" \
    || { echo "::error::relay never came up"; cat "$scratch/relay.log"; exit 1; }

echo "== start the native peer"
LAIT_NETWORK=local LAIT_RELAY="$relay" \
    "$root/target/debug/examples/live_echo" >"$scratch/peer.log" 2>&1 &
peer_pid=$!
peer_id=""
for _ in $(seq 1 100); do
    peer_id="$(sed -n 's/^device id //p' "$scratch/peer.log" | head -1)"
    [ -n "$peer_id" ] && break
    kill -0 "$peer_pid" 2>/dev/null || { echo "::error::peer exited"; cat "$scratch/peer.log"; exit 1; }
    perl -e 'select undef, undef, undef, 0.2'
done
[ -n "$peer_id" ] || { echo "::error::peer never announced its id"; cat "$scratch/peer.log"; exit 1; }
echo "   peer ${peer_id}"

echo "== the browser reaches it through the relay"
touch "$root/wasm-probe/tests/live.rs"
(
    cd "$root/wasm-probe"
    LIVE_RELAY_URL="$relay" LIVE_PEER_ID="$peer_id" \
        wasm-pack test --headless --chrome --test live --features probe-comms
)
echo "browser-live: the tab reached the native peer through lait's relay."
