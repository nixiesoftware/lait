#!/usr/bin/env bash
# The browser-ACCEPT spike: can a wasm iroh endpoint ACCEPT an incoming
# relay-routed connection — not just dial one? `browser-live.sh` proved the
# dial (browser dials, native echoes). This proves the ACCEPT, the reverse:
#
#   lait-relay (plain HTTP)  ⇐ ws ⇒  browser worker (comms/iroh, wasm32)  [ACCEPTS]
#            ⇑ ws ⇓
#   comms/examples/live_dialer (native, LAIT_NETWORK=local)              [DIALS]
#
# That one fact decides whether a browser tab can serve peers itself — hold the
# Contact responder / Live-plane accept role a daemon holds — or whether that
# role must live in a cloud/Pi companion. In gradient-topology terms: whether a
# tab's fan-out utility can be nonzero at all.
#
# The rendezvous needs no coordination: both halves share a fixed browser seed
# ([11u8;32]), so the dialer derives the tab's peer id itself and the tab needs
# to announce nothing. The relay URL is baked into the wasm test at compile
# time (wasm tests carry no runtime environment); the test source is touched
# first so a stale build can never carry yesterday's relay.
#
# Ordering: the tab's FIRST wasm compile can take minutes, so the dialer is
# started FIRST, in the background, retrying against a wall-clock deadline that
# spans the cold build — it must still be knocking when the tab finally launches
# and accepts, or the tab times its own accept out with nothing on the wire.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

# Skip-honesty: a skip is "could not be asked", never a pass.
if ! command -v wasm-pack >/dev/null 2>&1; then
    echo "browser-accept-spike: SKIPPED — wasm-pack is not installed." >&2
    exit 0
fi
if [ -z "${CHROME:-}" ] \
    && ! command -v google-chrome >/dev/null 2>&1 \
    && ! command -v chromium >/dev/null 2>&1 \
    && [ ! -e "/Applications/Google Chrome.app" ]; then
    echo "browser-accept-spike: SKIPPED — no Chrome found." >&2
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
    echo "browser-accept-spike: SKIPPED — no clang with a wasm backend (ring needs one)." >&2
    exit 0
fi
export CC_wasm32_unknown_unknown="$wasm_cc"
ar_dir="$(dirname "$wasm_cc")"
[ -x "$ar_dir/llvm-ar" ] && export AR_wasm32_unknown_unknown="$ar_dir/llvm-ar"

echo "== build the native half"
cargo build -p lait-relay --quiet
cargo build -p comms --example live_dialer --quiet

scratch="$(mktemp -d)"
relay_pid=""
dialer_pid=""
cleanup() {
    [ -n "$dialer_pid" ] && { kill "$dialer_pid" 2>/dev/null; wait "$dialer_pid" 2>/dev/null; } || true
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

# Start the DIALER first and let it retry through the tab's cold compile. It
# derives the tab's peer id from the shared browser seed, so it needs nothing
# from the tab — only the relay to route through.
echo "== start the native dialer (retrying until the tab accepts)"
LAIT_NETWORK=local LAIT_RELAY="$relay" ACCEPT_SPIKE_DEADLINE_SECS="${ACCEPT_SPIKE_DEADLINE_SECS:-420}" \
    "$root/target/debug/examples/live_dialer" >"$scratch/dialer.log" 2>&1 &
dialer_pid=$!

echo "== the browser ACCEPTS the native dial through the relay"
touch "$root/wasm-probe/tests/live_accept.rs"
accept_ok=0
# No portable `timeout` on macOS, so background wasm-pack under a watchdog that
# kills it past the deadline — a hung accept must not wedge the harness.
(
    cd "$root/wasm-probe"
    LIVE_RELAY_URL="$relay" \
        exec wasm-pack test --headless --chrome --test live_accept --features probe-comms
) &
wasm_pid=$!
( sleep 480; kill "$wasm_pid" 2>/dev/null ) &
watchdog=$!
if wait "$wasm_pid"; then accept_ok=1; fi
kill "$watchdog" 2>/dev/null || true
wait "$watchdog" 2>/dev/null || true

# The tab's accept loop ends when the dialer drops its stream (clean finish), so
# by the time the wasm test returns the dialer has already exited. Reap it and
# make its verdict authoritative: the "ACCEPT-SPIKE OK" line is the proof the
# tab accepted a real relay-routed dial and echoed a frame back.
dialer_rc=0
wait "$dialer_pid" 2>/dev/null || dialer_rc=$?
dialer_pid=""

if [ "$accept_ok" -eq 1 ] && [ "$dialer_rc" -eq 0 ] \
    && grep -q "ACCEPT-SPIKE OK" "$scratch/dialer.log"; then
    echo
    echo "browser-accept-spike: A wasm tab ACCEPTED a relay-routed inbound dial."
    echo "  → a browser tab's fan-out utility is nonzero: it can serve a peer,"
    echo "    not only reach one. The gradient can place a tab as a small anchor."
    exit 0
fi

echo "::error::the browser did not accept the dial"
echo "--- dialer.log ---"; cat "$scratch/dialer.log" || true
echo "--- relay.log (tail) ---"; tail -n 40 "$scratch/relay.log" || true
echo "(wasm accept test rc: ${accept_ok}=1-means-passed; dialer rc: ${dialer_rc})"
exit 1
