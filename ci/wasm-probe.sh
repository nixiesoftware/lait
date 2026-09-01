#!/usr/bin/env bash
# Does the engine reach the browser target? Three claims, checked in order:
#
#   1. the store stack (mechanics, journal, fabric, replica) compiles for
#      wasm32-unknown-unknown;
#   2. the transport (comms, and iroh behind it) compiles too;
#   3. the CRDT core RUNS in a JS host — fork, concurrent edit, exchange,
#      converge — via wasm-pack's Node runner.
#
# Claim 2 needs a C compiler that can emit wasm32 objects, because iroh's TLS
# pulls `ring`, whose build compiles C. Apple clang has no wasm backend; this
# script looks for one that does and SKIPS the claim, saying so, when none is
# found. A skip is "could not be asked", never "passes" — do not fold them.
set -euo pipefail
cd "$(dirname "$0")/../wasm-probe"

if ! rustup target list --installed | grep -q '^wasm32-unknown-unknown$'; then
    echo "wasm-probe: installing the wasm32-unknown-unknown target" >&2
    rustup target add wasm32-unknown-unknown
fi

echo "== claim 1: the store stack compiles for wasm32-unknown-unknown"
cargo check --target wasm32-unknown-unknown

echo "== claim 2: the transport compiles for wasm32-unknown-unknown"
wasm_cc=""
for candidate in "${CC_wasm32_unknown_unknown:-}" /opt/homebrew/opt/llvm@21/bin/clang \
    /opt/homebrew/opt/llvm/bin/clang /usr/bin/clang clang; do
    [ -n "$candidate" ] || continue
    if command -v "$candidate" >/dev/null 2>&1 \
        && "$candidate" --print-targets 2>/dev/null | grep -qi wasm; then
        wasm_cc="$(command -v "$candidate")"
        break
    fi
done
if [ -n "$wasm_cc" ]; then
    ar_dir="$(dirname "$wasm_cc")"
    [ -x "$ar_dir/llvm-ar" ] && export AR_wasm32_unknown_unknown="$ar_dir/llvm-ar"
    CC_wasm32_unknown_unknown="$wasm_cc" \
        cargo check --target wasm32-unknown-unknown --features probe-comms
else
    echo "wasm-probe: SKIPPED — no clang with a wasm backend found, so ring" >&2
    echo "wasm-probe: cannot build and the transport claim was not checked." >&2
fi

echo "== claim 3: the CRDT core runs in a JS host"
if command -v wasm-pack >/dev/null 2>&1; then
    wasm-pack test --node
else
    echo "wasm-probe: SKIPPED — wasm-pack is not installed, so the runtime" >&2
    echo "wasm-probe: claim was not checked. cargo install wasm-pack, or" >&2
    echo "wasm-probe: https://rustwasm.github.io/wasm-pack/installer/" >&2
fi
