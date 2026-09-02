#!/usr/bin/env bash
# Does the engine reach the browser target? Seven claims, checked in order:
#
#   1. the store stack (mechanics, journal, fabric, replica) compiles for
#      wasm32-unknown-unknown;
#   2. the transport (comms, iroh behind it) and the Contact pull compile too;
#   3. the CRDT core RUNS in a JS host — fork, concurrent edit, exchange,
#      converge — via wasm-pack's Node runner;
#   4. the OPFS medium carries the store in a real headless Chrome worker;
#   5. the World runner stack (runtime's guest carve, world-sdk, the Issues
#      World and its runner binary) compiles for the same target;
#   6. a World runner's four-function ABI RUNS in a real browser worker, with
#      the browser's own WebAssembly as the host in place of wasmtime;
#   7. the REAL Issues runner (typst, CRDT — 39 MiB) instantiates under that
#      browser WebAssembly: it compiles, fits, and its imports resolve.
#
# Claims 2 and 5 need a C compiler that can emit wasm32 objects, because
# iroh's TLS pulls `ring`, whose build compiles C. Apple clang has no wasm
# backend; this script looks for one that does and SKIPS the claim, saying
# so, when none is found. A skip is "could not be asked", never "passes" —
# do not fold them.
set -euo pipefail
cd "$(dirname "$0")/../wasm-probe"

if ! rustup target list --installed | grep -q '^wasm32-unknown-unknown$'; then
    echo "wasm-probe: installing the wasm32-unknown-unknown target" >&2
    rustup target add wasm32-unknown-unknown
fi

echo "== claim 1: the store stack compiles (and lints) for wasm32-unknown-unknown"
# clippy, not check: the OPFS medium exists only on this target, and a lint
# wall that never compiles a module is not covering it.
cargo clippy --target wasm32-unknown-unknown

echo "== claim 2: the transport and the Contact pull compile for wasm32-unknown-unknown"
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
        cargo check --target wasm32-unknown-unknown --features probe-contact
else
    echo "wasm-probe: SKIPPED — no clang with a wasm backend found, so ring" >&2
    echo "wasm-probe: cannot build and the transport claim was not checked." >&2
fi

echo "== claim 3: the CRDT core runs in a JS host"
if command -v wasm-pack >/dev/null 2>&1; then
    wasm-pack test --node --test smoke
else
    echo "wasm-probe: SKIPPED — wasm-pack is not installed, so the runtime" >&2
    echo "wasm-probe: claim was not checked. cargo install wasm-pack, or" >&2
    echo "wasm-probe: https://rustwasm.github.io/wasm-pack/installer/" >&2
fi

echo "== claim 4: the OPFS medium carries the store in a real browser worker"
if ! command -v wasm-pack >/dev/null 2>&1; then
    echo "wasm-probe: SKIPPED — wasm-pack is not installed (see claim 3)." >&2
elif [ -z "${CHROME:-}" ]     && ! command -v google-chrome >/dev/null 2>&1     && ! command -v chromium >/dev/null 2>&1     && [ ! -e "/Applications/Google Chrome.app" ]; then
    echo "wasm-probe: SKIPPED — no Chrome found, so the OPFS claim was not" >&2
    echo "wasm-probe: checked. A skip is 'could not be asked', never a pass." >&2
else
    wasm-pack test --headless --chrome --test opfs
fi

echo "== claim 5: the World runner stack compiles for wasm32-unknown-unknown"
# The guest carve: runtime's contract surface (world/exec/find/publication and
# what they stand on), the SDK's typed operations, the Issues World, and the
# runner binary itself all reach the browser target — the runner's wasm entry
# is the four-function guest ABI (`world_runner::export_world_runner!`). This
# claim proves it COMPILES; that the ABI actually carries a request and a host
# callback is proven natively by `world-runner-wasm`'s proof test, which runs a
# proof-World module under wasmtime in the ordinary engine suite. The in-Worker
# execution of the REAL Issues runner (typst, CRDT, under real limits) is a
# later slice; a dependency regression that knocks the runner off the target
# fails here, not there.
if [ -n "$wasm_cc" ]; then
    CC_wasm32_unknown_unknown="$wasm_cc" \
        RUSTFLAGS='--cfg getrandom_backend="wasm_js"' \
        cargo check --manifest-path ../Cargo.toml --target wasm32-unknown-unknown \
        -p runtime -p world-sdk -p lait-issues -p lait-issues-runner
else
    echo "wasm-probe: SKIPPED — no clang with a wasm backend found (see" >&2
    echo "wasm-probe: claim 2), so the runner-stack claim was not checked." >&2
fi

echo "== claim 6: a World runner ABI runs in a real browser, host = the browser"
# The native wasmtime host proves the four-function ABI carries a request and a
# host callback (claim 5's sibling, world-runner-wasm's proof test). This runs
# the SAME proof-World in a headless-Chrome Worker, where the host that runs the
# guest wasm is the browser's own WebAssembly and JS glue — no wasmtime. It is
# the mechanism the in-browser engine uses to run a World runner. Needs a
# wasm-capable clang (proof-world links world-runner, which does not pull ring,
# but the probe crate's own graph might); guarded like claims 4/2.
if ! command -v wasm-pack >/dev/null 2>&1; then
    echo "wasm-probe: SKIPPED — wasm-pack is not installed (see claim 3)." >&2
elif [ -z "${CHROME:-}" ] \
    && ! command -v google-chrome >/dev/null 2>&1 \
    && ! command -v chromium >/dev/null 2>&1 \
    && [ ! -e "/Applications/Google Chrome.app" ]; then
    echo "wasm-probe: SKIPPED — no Chrome found, so the browser-runner claim" >&2
    echo "wasm-probe: was not checked. A skip is 'could not be asked', not a pass." >&2
else
    ${wasm_cc:+CC_wasm32_unknown_unknown="$wasm_cc"} \
        wasm-pack test --headless --chrome --no-default-features --features probe-runner --test runner
fi

echo "== claim 7: the real Issues runner instantiates under browser WebAssembly"
# The plan-invalidator for browser execution: a 39 MiB typst/CRDT World runner
# must compile, fit, and instantiate in a tab. The runner links no iroh on wasm
# (comms is native-only; contact's `wire` is off) and takes entropy from a
# `lait.random` host import, so it is a near-pure core-wasm module needing no
# wasm-bindgen runtime to load. The harness builds it here (minutes; needs the
# wasm clang) and hands the path to the browser test.
if ! command -v wasm-pack >/dev/null 2>&1; then
    echo "wasm-probe: SKIPPED — wasm-pack is not installed (see claim 3)." >&2
elif [ -z "$wasm_cc" ]; then
    echo "wasm-probe: SKIPPED — no wasm clang (see claim 2), so the Issues" >&2
    echo "wasm-probe: runner was not built and the claim was not checked." >&2
elif [ -z "${CHROME:-}" ] \
    && ! command -v google-chrome >/dev/null 2>&1 \
    && ! command -v chromium >/dev/null 2>&1 \
    && [ ! -e "/Applications/Google Chrome.app" ]; then
    echo "wasm-probe: SKIPPED — no Chrome found, so the claim was not checked." >&2
else
    runner_wasm="$(cd .. && pwd)/target/wasm32-unknown-unknown/release/lait_issues_runner.wasm"
    CC_wasm32_unknown_unknown="$wasm_cc" \
        RUSTFLAGS='--cfg getrandom_backend="custom"' \
        cargo build --manifest-path ../Cargo.toml --release \
        --target wasm32-unknown-unknown -p lait-issues-runner
    CC_wasm32_unknown_unknown="$wasm_cc" ISSUES_RUNNER_WASM="$runner_wasm" \
        wasm-pack test --headless --chrome --no-default-features --features probe-runner --test issues_runner
fi
