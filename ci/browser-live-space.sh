#!/usr/bin/env bash
# The 4b claim, end to end: a browser worker joins a real Space as a
# pre-admitted member device and pulls it over lait/contact/2 from a real
# daemon — through lait's own relay, onto real OPFS, with the production
# Contact grammar. Three native processes (relay, alice's daemon, briefly
# bob's), one headless Chrome; nothing mocked, nothing public.
#
# The pre-admission trick: the daemon loads an existing secret.key before
# minting one, so this harness CHOOSES bob's seed, lets a scratch daemon
# redeem the invite natively (creating the admitted actor + sealed keys in
# the replicated ledger), stops it — one DeviceId, one holder — and hands
# the browser the seed + the ticket. The browser is then a member device
# pulling, never a second self-inception.
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
    for node in ALICE BOB; do
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

# --- bob: OUR seed, native admission, then gone ----------------------------
SEED_HEX="$(openssl rand -hex 32)"
mkdir -p "$ROOT/bob/config"
printf '%s' "$SEED_HEX" > "$ROOT/bob/config/secret.key"
start_node bob
entered="$(post "$BOB_TOKEN" "$BOB_PORT" /api/host/rpc \
    "{\"cmd\":\"host_space_enter\",\"link\":\"$LINK\",\"home\":\"$ROOT/bob/space/.lait\",\"nick\":\"bob\"}")"
has "$entered" '"host":"entered"'

seen=""
for _ in $(seq 1 60); do
    members="$(post "$ALICE_TOKEN" "$ALICE_PORT" "/api/spaces/$AORB/rpc" '{"cmd":"members"}')"
    actors="$(printf '%s' "$members" | grep -o '"key":"act_' | wc -l | tr -d ' ')"
    case "$members" in
        *'"role":"member"'*) [ "$actors" -ge 2 ] && { seen=yes; break; } ;;
    esac
    sleep 1
done
[ -n "$seen" ] || { echo "::error::alice never saw the second member:"; echo "$members"; exit 1; }

# One DeviceId, one holder: bob's daemon stops before the browser continues
# his identity. host_restart stops the daemon under the head; killing the head
# right after means nothing stands it back up.
post "$BOB_TOKEN" "$BOB_PORT" /api/host/rpc '{"cmd":"host_restart"}' >/dev/null 2>&1 || true
kill "$BOB_PID" 2>/dev/null || true
wait "$BOB_PID" 2>/dev/null || true
BOB_TOKEN=""

# --- the browser pulls ------------------------------------------------------
echo "== the browser pulls the Space through the relay"
touch "$root/wasm-probe/tests/space.rs"
(
    cd "$root/wasm-probe"
    LIVE_RELAY_URL="$relay" LIVE_SEED_HEX="$SEED_HEX" LIVE_TICKET="$LINK" \
        LIVE_EXPECT_BODIES=3 \
        wasm-pack test --headless --chrome --test space --features probe-contact
)
echo "browser-live-space: the tab joined and pulled the Space over Contact."

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
