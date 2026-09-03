#!/usr/bin/env bash
# The datagram spike: does an unreliable datagram round-trip from a wasm engine
# in headless Chrome to a native peer through lait's own relay? This is the
# go/no-go for a p2p live-caret client — the Live plane carries carets only as
# datagrams, and the browser's only path is the relay's WebSocket.
#
#   lait-relay (plain HTTP)  ⇐ ws ⇒  browser worker (comms/iroh, wasm32)
#            ⇑ ws ⇓
#   comms/examples/live_datagram (native, LAIT_NETWORK=local)
#
# Structure mirrors ci/browser-live.sh (the stream claim); only the native
# example and the wasm test differ.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

if ! command -v wasm-pack >/dev/null 2>&1; then
    echo "browser-datagram: SKIPPED — wasm-pack is not installed." >&2
    exit 0
fi
if [ -z "${CHROME:-}" ] \
    && ! command -v google-chrome >/dev/null 2>&1 \
    && ! command -v chromium >/dev/null 2>&1 \
    && [ ! -e "/Applications/Google Chrome.app" ]; then
    echo "browser-datagram: SKIPPED — no Chrome found." >&2
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
    echo "browser-datagram: SKIPPED — no clang with a wasm backend (ring needs one)." >&2
    exit 0
fi
export CC_wasm32_unknown_unknown="$wasm_cc"
ar_dir="$(dirname "$wasm_cc")"
[ -x "$ar_dir/llvm-ar" ] && export AR_wasm32_unknown_unknown="$ar_dir/llvm-ar"

echo "== build the native half"
cargo build -p lait-relay --quiet
cargo build -p comms --example live_datagram --quiet

scratch="$(mktemp -d)"
relay_pid=""
peer_pid=""
cleanup() {
    [ -n "$peer_pid" ] && { kill "$peer_pid" 2>/dev/null; wait "$peer_pid" 2>/dev/null; } || true
    [ -n "$relay_pid" ] && { kill "$relay_pid" 2>/dev/null; wait "$relay_pid" 2>/dev/null; } || true
    rm -rf "$scratch"
}
trap cleanup EXIT

port=$(( (RANDOM % 20000) + 40000 ))
relay="http://127.0.0.1:${port}"

echo "== start the relay at ${relay}"
"$root/target/debug/lait-relay" --http "127.0.0.1:${port}" >"$scratch/relay.log" 2>&1 &
relay_pid=$!
curl -sf -o /dev/null --retry 20 --retry-connrefused --retry-delay 1 "$relay" \
    || { echo "::error::relay never came up"; cat "$scratch/relay.log"; exit 1; }

echo "== start the native datagram peer"
LAIT_NETWORK=local LAIT_RELAY="$relay" \
    "$root/target/debug/examples/live_datagram" >"$scratch/peer.log" 2>&1 &
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

echo "== the browser round-trips a datagram through the relay"
touch "$root/wasm-probe/tests/live_datagram.rs"
(
    cd "$root/wasm-probe"
    LIVE_RELAY_URL="$relay" LIVE_PEER_ID="$peer_id" \
        wasm-pack test --headless --chrome --test live_datagram --features probe-comms
)
echo "browser-datagram: a datagram round-tripped tab↔native through lait's relay."
echo "== native peer log (datagram capacity):"
grep -i "datagram capacity" "$scratch/peer.log" || true
