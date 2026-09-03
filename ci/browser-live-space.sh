#!/usr/bin/env bash
# The join-from-link claim, end to end: a browser worker holding nothing but
# a fresh seed and an invite link joins a real Space over lait/contact/2 from
# a real daemon — through lait's own relay, onto real OPFS, with the
# production Contact grammar. Two native processes (relay, alice's daemon),
# one headless Chrome; nothing mocked, nothing public, and NO daemon ever
# holds bob's seed: the tab self-incepts, pushes its pending admission
# request on its own dial, and alice's daemon redeems it — the admission
# courier for a device nothing can dial back.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

# Skip-honesty: a skip is "could not be asked", never a pass.
if ! command -v wasm-pack >/dev/null 2>&1; then
    echo "browser-live-space: SKIPPED — wasm-pack is not installed." >&2
    exit 0
fi
if [ -z "${CHROME:-}" ] \
    && ! command -v google-chrome >/dev/null 2>&1 \
    && ! command -v chromium >/dev/null 2>&1 \
    && [ ! -e "/Applications/Google Chrome.app" ]; then
    echo "browser-live-space: SKIPPED — no Chrome found." >&2
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
    echo "browser-live-space: SKIPPED — no clang with a wasm backend." >&2
    exit 0
fi
export CC_wasm32_unknown_unknown="$wasm_cc"
ar_dir="$(dirname "$wasm_cc")"
[ -x "$ar_dir/llvm-ar" ] && export AR_wasm32_unknown_unknown="$ar_dir/llvm-ar"

echo "== build the native half"
cargo build -p lait -p lait-relay -p lait-feed -p world-channel-installer --quiet

ROOT="$(mktemp -d)"
export LAIT_DISPLAY=off
export LAIT_IDLE_SECS=0
unset LAIT_HOME LAIT_STORE LAIT_AGENT LAIT_AS || true

PIDS=()
cleanup() {
    for node in ALICE; do
        token="$(eval "printf '%s' \"\${${node}_TOKEN:-}\"")"
        port="$(eval "printf '%s' \"\${${node}_PORT:-}\"")"
        [ -n "$token" ] && [ -n "$port" ] && curl -sS -m 5 -X POST \
            "http://127.0.0.1:${port}/api/host/rpc" \
            -H "Authorization: Bearer ${token}" -H "content-type: application/json" \
            -d '{"cmd":"host_restart"}' >/dev/null 2>&1 || true
    done
    for pid in "${PIDS[@]:-}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
    rm -rf "$ROOT"
}
trap cleanup EXIT

# --- the relay -------------------------------------------------------------
port=$(( (RANDOM % 20000) + 40000 ))
relay="http://127.0.0.1:${port}"
echo "== start the relay at ${relay}"
"$root/target/debug/lait-relay" --http "127.0.0.1:${port}" >"$ROOT/relay.log" 2>&1 &
PIDS+=("$!")
curl -sf -o /dev/null --retry 20 --retry-connrefused --retry-delay 1 "$relay" \
    || { echo "::error::relay never came up"; cat "$ROOT/relay.log"; exit 1; }
export LAIT_NETWORK=local
export LAIT_RELAY="$relay"

has() { case "$1" in *"$2"*) : ;; *) echo "::error::expected '$2' in:"; echo "$1"; exit 1 ;; esac; }

# A clean identity has no installed Worlds and `lait --json` refuses to start
# with none, so each node gets the signed Issues fixture through the
# production installer — the same preamble the smoke runs.
if [ -z "${WORLD_FIXTURE_CHANNELS:-}" ]; then
    echo "== publish the signed World fixture channels"
    WORLD_FIXTURE_CHANNELS="$ROOT/channels"
    bash "$root/ci/prepare-independent-world-fixtures.sh" \
        "$WORLD_FIXTURE_CHANNELS" "$root/target/debug" "$root/target/debug/lait-feed"
fi
WORLD_FIXTURE_INSTALLER="${WORLD_FIXTURE_INSTALLER:-$root/target/debug/world-channel-installer}"

start_node() { # $1 = node name; sets <NAME>_TOKEN / <NAME>_PORT / <NAME>_PID
    local name="$1"
    local ready="$ROOT/$name.ready.json"
    "$WORLD_FIXTURE_INSTALLER" \
        --channels "$WORLD_FIXTURE_CHANNELS" \
        --identity "$ROOT/$name/config" \
        --world com.lait.issues
    LAIT_CONFIG_ROOT="$ROOT/$name/config" "$root/target/debug/lait" --json --port 0 \
        >"$ready" 2>"$ROOT/$name.head.log" &
    local pid="$!"
    PIDS+=("$pid")
    for _ in $(seq 1 200); do
        [ -s "$ready" ] && break
        sleep 0.25
    done
    [ -s "$ready" ] || { echo "::error::$name never announced:"; cat "$ROOT/$name.head.log"; exit 1; }
    local upper; upper="$(printf '%s' "$name" | tr '[:lower:]' '[:upper:]')"
    local field; field() { sed -n "s/.*\"$1\":[[:space:]]*\"\{0,1\}\([^\",}]*\)\"\{0,1\}.*/\1/p" "$ready"; }
    eval "${upper}_TOKEN=\"\$(field token)\""
    eval "${upper}_PORT=\"\$(field port)\""
    eval "${upper}_PID=\"$pid\""
}

post() { # $1 token, $2 port, $3 path, $4 body
    curl -sS --fail-with-body -X POST "http://127.0.0.1:$2$3" \
        -H "Authorization: Bearer $1" \
        -H "content-type: application/json" \
        -d "$4"
}

# --- alice founds, writes, invites -----------------------------------------
start_node alice
founded="$(post "$ALICE_TOKEN" "$ALICE_PORT" /api/host/rpc \
    "{\"cmd\":\"host_space_found\",\"home\":\"$ROOT/alice/space/.lait\",\"name\":\"Live\",\"nick\":\"alice\"}")"
has "$founded" '"host":"founded"'
spaces="$(curl -sS --fail-with-body "http://127.0.0.1:${ALICE_PORT}/api/spaces" \
    -H "Authorization: Bearer $ALICE_TOKEN")"
AORB="$(printf '%s' "$spaces" | sed -n 's/.*"id":[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
[ -n "$AORB" ] || { echo "::error::no orbit id in:"; echo "$spaces"; exit 1; }

post "$ALICE_TOKEN" "$ALICE_PORT" "/api/spaces/$AORB/worlds/issues/rpc" \
    '{"cmd":"project_new","name":"Engineering","key":"ENG"}' >/dev/null
post "$ALICE_TOKEN" "$ALICE_PORT" "/api/spaces/$AORB/worlds/issues/rpc" \
    '{"cmd":"issue_new","title":"the tab pulls this issue","project":"ENG"}' >/dev/null
post "$ALICE_TOKEN" "$ALICE_PORT" "/api/spaces/$AORB/worlds/issues/rpc" \
    '{"cmd":"issue_new","title":"and this one","project":"ENG"}' >/dev/null

invited="$(post "$ALICE_TOKEN" "$ALICE_PORT" "/api/spaces/$AORB/rpc" \
    '{"cmd":"invite","role":"contributor","reusable":false,"ttl_hours":1}')"
TICKET="$(printf '%s' "$invited" | sed -n 's/.*"reff":[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
[ -n "$TICKET" ] || { echo "::error::no ticket in:"; echo "$invited"; exit 1; }
LINK="lait://join/$TICKET"

# --- bob is ONLY a tab: a fresh seed no daemon ever holds -------------------
SEED_HEX="$(openssl rand -hex 32)"

# --- the browser enters and pulls -------------------------------------------
echo "== the browser enters the Space from the invite alone"
touch "$root/wasm-probe/tests/space.rs"
(
    cd "$root/wasm-probe"
    LIVE_RELAY_URL="$relay" LIVE_SEED_HEX="$SEED_HEX" LIVE_TICKET="$LINK" \
        LIVE_EXPECT_BODIES=3 \
        wasm-pack test --headless --chrome --test space --features probe-contact
)
echo "browser-live-space: the tab entered and pulled the Space over Contact."

# The founder-side proof of the in-tab admission: alice's ledger now carries
# bob's actor as an admitted member — redeemed from the request the tab
# pushed, since no daemon ever ran bob's enter.
seen=""
for _ in $(seq 1 30); do
    members="$(post "$ALICE_TOKEN" "$ALICE_PORT" "/api/spaces/$AORB/rpc" '{"cmd":"members"}')"
    actors="$(printf '%s' "$members" | grep -o '"key":"act_' | wc -l | tr -d ' ')"
    case "$members" in
        *'"role":"member"'*) [ "$actors" -ge 2 ] && { seen=yes; break; } ;;
    esac
    sleep 1
done
[ -n "$seen" ] || { echo "::error::alice never admitted the tab's actor:"; echo "$members"; exit 1; }
echo "browser-live-space: alice's ledger admitted the actor the tab incepted."

# --- the engine answers from the pulled Space -------------------------------
# The same pull, then the daemon's own Session machinery composed over it
# (`runtime::browser`, LedgerAuthorityView over the pulled ledger), querying
# the real Issues runner — the issues alice wrote come back in a tab.
echo "== build the Issues runner wasm module"
runner_wasm="$root/target/wasm32-unknown-unknown/release/lait_issues_runner.wasm"
RUSTFLAGS='--cfg getrandom_backend="custom"' \
    cargo build --manifest-path "$root/Cargo.toml" --release \
    --target wasm32-unknown-unknown -p lait-issues-runner --quiet
echo "== the engine in the tab answers a real query from the pulled Space"
touch "$root/wasm-probe/tests/space_call.rs"
(
    cd "$root/wasm-probe"
    LIVE_RELAY_URL="$relay" LIVE_SEED_HEX="$SEED_HEX" LIVE_TICKET="$LINK" \
        ISSUES_RUNNER_WASM="$runner_wasm" \
        wasm-pack test --headless --chrome --test space_call \
        --features probe-engine,probe-contact
)
echo "browser-live-space: the engine in the tab answered from the pulled Space."

# --- the tab's write converges OUT to alice's daemon (symmetric Contact) -----
# space_call.rs had bob's tab PUSH its write on the dial it made. Alice's daemon
# never dials the tab; the only way it can hold bob's issue is the reverse
# incorporate. Poll alice until it appears — the responder incorporates
# asynchronously after acking receipt.
echo "== the tab's write converged OUT to alice's daemon"
seen_push=""
for _ in $(seq 1 30); do
    issues="$(post "$ALICE_TOKEN" "$ALICE_PORT" "/api/spaces/$AORB/worlds/issues/rpc" \
        '{"cmd":"list","page":{}}')" || true
    case "$issues" in
        *"written from a tab"*) seen_push=yes; break ;;
    esac
    sleep 1
done
[ -n "$seen_push" ] || {
    echo "::error::alice's daemon never received the tab's pushed write:"
    echo "$issues"
    exit 1
}
echo "browser-live-space: the tab's write converged OUT to alice's daemon."

# --- a product world RPC crosses the world-agnostic dispatch seam -----------
# parse_web → execute → the runner's callbacks → the composed Session, naming
# no World — and the runner re-enters itself through call_world. The settling
# proof that a product request (not a raw semantic Query) crosses in a tab.
echo "== a product world RPC crosses the dispatch seam in the tab"
touch "$root/wasm-probe/tests/dispatch.rs"
(
    cd "$root/wasm-probe"
    LIVE_RELAY_URL="$relay" LIVE_SEED_HEX="$SEED_HEX" LIVE_TICKET="$LINK" \
        ISSUES_RUNNER_WASM="$runner_wasm" \
        wasm-pack test --headless --chrome --test dispatch \
        --features probe-dispatch
)
echo "browser-live-space: a product world RPC crossed the dispatch seam."

# --- the shippable packaging boundary: boot() + the handle ------------------
# One `boot` call stands the whole engine up and returns the `#[wasm_bindgen]`
# handle the viewer's Worker holds; it then answers frames as JSON strings and
# installs a live re-pull. The settling proof that the engine PACKAGES — a
# non-Send handle survives a return to JS and a later call back in.
echo "== the packaging boundary boots the engine and answers a frame in the tab"
touch "$root/wasm-probe/tests/handle.rs"
(
    cd "$root/wasm-probe"
    LIVE_RELAY_URL="$relay" LIVE_SEED_HEX="$SEED_HEX" LIVE_TICKET="$LINK" \
        ISSUES_RUNNER_WASM="$runner_wasm" \
        wasm-pack test --headless --chrome --test handle \
        --features probe-dispatch
)
echo "browser-live-space: the packaging boundary booted the engine in the tab."

# --- the session/editor lane answers in the tab -----------------------------
# The Worker-side session host: workerSession.ts's exact frame vocabulary over
# the composed engine — liveness on open, the daemon's editor allowlist, a
# CRDT text splice landing with its operation envelope intact, clone-safe
# refusals, sid scoping, and a read of converged state after a re-pull.
echo "== the session/editor lane answers in the tab"
touch "$root/wasm-probe/tests/session.rs"
(
    cd "$root/wasm-probe"
    LIVE_RELAY_URL="$relay" LIVE_SEED_HEX="$SEED_HEX" LIVE_TICKET="$LINK" \
        ISSUES_RUNNER_WASM="$runner_wasm" \
        wasm-pack test --headless --chrome --test session \
        --features probe-dispatch
)
echo "browser-live-space: the session/editor lane answered in the tab."
