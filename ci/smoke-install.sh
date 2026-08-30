#!/usr/bin/env bash
#
# smoke-install.sh — the install line, end to end, on a real Linux box.
#
# Every part of the headless install has unit tests; what none of them reach is
# the composition, which is the half this tree keeps getting wrong while every
# part is correct (CLAUDE.md, `tools/astrolabe/tests/launch.rs`). So this stands
# up the whole chain against a scratch feed nobody but this run trusts:
#
#   a signed scratch feed served over HTTP
#     → `install.sh` fetched and piped to sh, inside a systemd container
#       → `lait install` proves the channel and writes /var/lib/lait + the unit
#         → the daemon boots supervised, shows a pairing code, records a standing
#           → a newer release is published
#             → the daemon's own watcher proves it, swaps its binary, and exits
#               → systemd restarts it exactly once, on the release it took
#
# The scratch feed is why `LAIT_FEED_BASE_URL`/`LAIT_FEED_PUBKEYS` exist, and
# why they are read only by a debug build: the binary this publishes is built
# `--debug` on purpose, and a release binary would ignore both and go to the
# real bucket. Nothing here touches gs://the-foundation-dist.
#
# Requires a Docker daemon that can run privileged Linux containers with
# cgroups (an ubuntu CI runner, a Linux workstation, or Docker Desktop with the
# daemon up). It is not runnable on a Mac with Docker Desktop stopped, which is
# where it was written; CI's ubuntu job is where it runs unattended.
#
#   bash ci/smoke-install.sh                 # build the Linux binary in a container
#   bash ci/smoke-install.sh --tarball path  # take a prebuilt lait-<triple>.tar.gz
#   bash ci/smoke-install.sh --keep          # leave the containers up to poke at
set -euo pipefail

TARBALL="" KEEP=""
while [ $# -gt 0 ]; do
  case "$1" in
    --tarball) TARBALL="$2"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    *) echo "smoke-install: unknown argument $1" >&2; exit 1 ;;
  esac
done

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
NET="lait-smoke-$$"
FEED_BOX="lait-smoke-feed-$$"
BOX="lait-smoke-box-$$"

cleanup() {
  if [ -n "$KEEP" ]; then
    echo "smoke-install: leaving $BOX and $FEED_BOX up"
    return 0
  fi
  docker rm -f "$BOX" "$FEED_BOX" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
  rm -rf "$WORK"
  return 0
}
trap cleanup EXIT

docker info >/dev/null 2>&1 || {
  echo "smoke-install: no Docker daemon; this needs one that can run privileged Linux containers" >&2
  exit 1
}

VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$REPO/Cargo.toml" | head -1)"
# The second publish only has to be NEWER than what is installed; it carries the
# same bytes. What the update half proves is the chain — prove, swap, exit,
# restart — not that two builds differ, which the digest check already settles.
NEXT="${VERSION%.*}.$(( ${VERSION##*.} + 1 ))"
TARGET="x86_64-unknown-linux-gnu"
case "$(docker version --format '{{.Server.Arch}}' 2>/dev/null || echo amd64)" in
  arm64|aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
esac
echo "smoke-install: $VERSION → $NEXT on $TARGET"

# ---------------------------------------------------------------------------
# 1. A Linux `lait`, in the archive layout `update::bin_path_for` extracts from.
#    Debug, because only a debug build will believe a scratch feed.
if [ -z "$TARBALL" ]; then
  echo "smoke-install: building lait for $TARGET (debug) in a container"
  docker run --rm \
    -v "$REPO":/src -w /src \
    -v lait-smoke-cargo:/usr/local/cargo/registry \
    -v lait-smoke-target:/target \
    -e CARGO_TARGET_DIR=/target \
    rust:1-bookworm \
    cargo build --locked -p lait --bin lait
  docker run --rm -v lait-smoke-target:/target -v "$WORK":/out \
    -e TARGET="$TARGET" debian:bookworm-slim \
    sh -c 'mkdir -p "/tmp/lait-$TARGET" \
      && cp /target/debug/lait "/tmp/lait-$TARGET/lait" \
      && tar czf "/out/lait-$TARGET.tar.gz" -C /tmp "lait-$TARGET"'
  TARBALL="$WORK/lait-$TARGET.tar.gz"
fi
[ -f "$TARBALL" ] || { echo "smoke-install: no archive at $TARBALL" >&2; exit 1; }

FEED_TOOL="${FEED_TOOL:-cargo run -q --manifest-path $REPO/Cargo.toml -p lait-feed --}"

# ---------------------------------------------------------------------------
# 2. A scratch feed: one seed, minted here and destroyed with the run.
FEED_DIR="$WORK/feed"
mkdir -p "$FEED_DIR/channels"
SEED="$WORK/seed"
# shellcheck disable=SC2086
PUBKEY="$($FEED_TOOL keygen --out "$SEED" | sed -n 's/^public key ([^)]*): //p')"
[ -n "$PUBKEY" ] || { echo "smoke-install: keygen printed no public key" >&2; exit 1; }
BASE="http://feed:8080"

publish() { # $1 version
  local version="$1"
  local dir="$FEED_DIR/releases/$version"
  mkdir -p "$dir"
  # Both Linux archives are the one binary we built: the container only ever
  # selects its own architecture, and the manifest requires every lait target
  # to exist, so the platforms this run cannot exercise get a placeholder.
  cp "$TARBALL" "$dir/lait-x86_64-unknown-linux-gnu.tar.gz"
  cp "$TARBALL" "$dir/lait-aarch64-unknown-linux-gnu.tar.gz"
  for absent in lait-aarch64-apple-darwin.tar.gz lait-x86_64-apple-darwin.tar.gz \
    lait-x86_64-pc-windows-msvc.zip; do
    printf 'not built for this smoke\n' > "$dir/$absent"
  done
  # shellcheck disable=SC2086
  $FEED_TOOL installer --version "$version" --base-url "$BASE" --artifacts-dir "$dir"
  # shellcheck disable=SC2086
  $FEED_TOOL manifest --version "$version" --base-url "$BASE" --artifacts-dir "$dir" \
    --seed "$SEED" --out "$dir/manifest.json"
  # shellcheck disable=SC2086
  $FEED_TOOL pointer --channel stable --version "$version" \
    --manifest-url "$BASE/releases/$version/manifest.json" \
    --manifest "$dir/manifest.json" --seed "$SEED" \
    --out "$FEED_DIR/channels/stable"
}

publish "$VERSION"

# ---------------------------------------------------------------------------
# 3. The feed host and the box.
docker network create "$NET" >/dev/null
docker run -d --name "$FEED_BOX" --network "$NET" --network-alias feed \
  -v "$FEED_DIR":/srv:ro -w /srv python:3-slim \
  python3 -m http.server 8080 >/dev/null

docker build -q -t lait-smoke-box - >/dev/null <<'IMAGE'
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends systemd systemd-sysv curl ca-certificates \
 && rm -rf /var/lib/apt/lists/*
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
IMAGE

docker run -d --name "$BOX" --network "$NET" --privileged --cgroupns=host \
  -v /sys/fs/cgroup:/sys/fs/cgroup:rw --tmpfs /run --tmpfs /run/lock \
  lait-smoke-box >/dev/null

in_box() { docker exec "$BOX" "$@"; }
# The trust root reaches a process through its environment, so the install line
# and the daemon are handed it two different ways: an exec env for the one, a
# systemd drop-in for the other. Neither is a production shape; both exist
# because a scratch feed is the only feed this run is allowed to touch.
scratch_env() { docker exec -e "LAIT_FEED_BASE_URL=$BASE" -e "LAIT_FEED_PUBKEYS=$PUBKEY" "$BOX" "$@"; }
wait_for() { # $1 seconds, $2... a command that must eventually succeed
  local deadline=$(( SECONDS + $1 )); shift
  until "$@" >/dev/null 2>&1; do
    [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep 2
  done
}

in_box systemctl is-system-running --wait >/dev/null 2>&1 \
  || echo "smoke-install: NOTE systemd reports degraded; continuing"

# The drop-in is written before the unit exists: systemd reads it at the
# daemon-reload `lait install` runs, so the very first boot already follows the
# scratch feed rather than reaching for the real bucket.
in_box mkdir -p /etc/systemd/system/lait.service.d
in_box sh -c "printf '[Service]\nEnvironment=LAIT_FEED_BASE_URL=$BASE LAIT_FEED_PUBKEYS=$PUBKEY\n' \
  > /etc/systemd/system/lait.service.d/scratch-feed.conf"

# ---------------------------------------------------------------------------
# 4. The install line, exactly as a person runs it.
scratch_env sh -c "curl -fsSL $BASE/releases/$VERSION/install.sh | sh" \
  || { echo "smoke-install: the install line failed" >&2; exit 1; }

in_box systemctl is-active lait >/dev/null \
  || { echo "smoke-install: lait is not active after install" >&2; in_box systemctl status lait; exit 1; }
in_box test -x /var/lib/lait/bin/lait
in_box test -f /var/lib/lait/bin/installed.json
echo "smoke-install: installed — $(in_box cat /var/lib/lait/bin/installed.json)"

# A box nobody owns yet says so, in the journal and for as long as it is unpaired.
wait_for 60 in_box sh -c 'journalctl -u lait --no-pager | grep -qi "pairing code"' \
  || { echo "smoke-install: the journal never showed a pairing code" >&2; in_box journalctl -u lait --no-pager | tail -40; exit 1; }

# The watcher's first check is spread over a minute, so this is the proof that a
# service installation stages at all — the defect S4 exists to fix.
wait_for 90 in_box test -f /var/lib/lait/update-standing.json \
  || { echo "smoke-install: no standing was recorded; the watcher never ran" >&2; in_box journalctl -u lait --no-pager | tail -40; exit 1; }
echo "smoke-install: standing — $(in_box cat /var/lib/lait/update-standing.json)"

# ---------------------------------------------------------------------------
# 5. A newer release, taken by the daemon itself.
publish "$NEXT"
# In production a notify announcement wakes the watcher inside the round trip
# and the 4.5 h period is only the floor. Neither is worth a relay container
# here: a manual restart puts the next check inside a minute, and everything
# after it — prove, swap, exit, be restarted — is the path a real update takes.
# `systemctl restart` is not an automatic restart, so it leaves NRestarts alone
# and the count below still means "the swap asked for a fresh generation".
in_box systemctl restart lait
BEFORE="$(in_box systemctl show -p NRestarts --value lait)"

wait_for 120 in_box sh -c "grep -q '$NEXT' /var/lib/lait/update-standing.json" \
  || { echo "smoke-install: the daemon never took $NEXT" >&2; in_box journalctl -u lait --no-pager | tail -60; exit 1; }

restarts_past() { [ "$(in_box systemctl show -p NRestarts --value lait)" -gt "$BEFORE" ]; }
wait_for 60 restarts_past \
  || { echo "smoke-install: the swap never asked systemd for a fresh generation" >&2; exit 1; }
AFTER="$(in_box systemctl show -p NRestarts --value lait)"
[ "$AFTER" -eq $(( BEFORE + 1 )) ] \
  || { echo "smoke-install: expected exactly one automatic restart, got $(( AFTER - BEFORE ))" >&2; exit 1; }
in_box systemctl is-active lait >/dev/null \
  || { echo "smoke-install: lait did not come back on the release it took" >&2; exit 1; }

echo "smoke-install: the install line installed $VERSION, took $NEXT, and came back once"
