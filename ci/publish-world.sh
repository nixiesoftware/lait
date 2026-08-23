#!/usr/bin/env bash
#
# publish-world.sh — ship a World in one act (SUB-23).
#
# The goal Spec says shipping is one act. This is that act: a developer runs
# one command and a World's bundle is live on its test channel; promoting it
# to stable is the same command with `--promote`, which rewrites one file and
# rebuilds nothing.
#
# The one law of motion is the product feed's, one level in: every artifact
# and the signed manifest are uploaded and read back over the public door
# before the channel pointer that names them is rewritten. A pointer naming
# objects that are not there yet is a broken World for every follower at once.
# The rules that must fail a publish — a stable pointer naming a prerelease, a
# manifest the signing key never sealed — live in `lait-feed world`, which
# refuses to seal; this script only sequences uploads.
#
# Usage:
#   ci/publish-world.sh --world com.lait.issues --version 0.1.0 \
#     --bundle x86_64-unknown-linux-gnu=target/linux/issues \
#     --bundle x86_64-pc-windows-msvc=target/windows/issues \
#     --channel test --seed ~/.lait-feed-signing.seed
#
#   ci/publish-world.sh --world com.lait.issues --version 0.1.0 \
#     --channel stable --promote --seed ~/.lait-feed-signing.seed
#
# The bundle must carry `world.json` at its root: what the World is, how to
# reach it, and the host facts it runs against. `lait-feed world` refuses
# without one, so a publisher learns at publish time rather than a machine
# learning after it has already downloaded.
#
# Requires: gcloud (authenticated with write access to the bucket), curl, and
# a built `lait-feed` (cargo build -p lait-feed).
#
# The seed never leaves the machine invoking this script.

set -euo pipefail

BUCKET="gs://the-foundation-dist"
BASE_URL="https://storage.googleapis.com/the-foundation-dist"

WORLD="" VERSION="" CHANNEL="" SEED="" PROMOTE=""
BUNDLES=()
while [ $# -gt 0 ]; do
  case "$1" in
    --world) WORLD="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --bundle) BUNDLES+=("$2"); shift 2 ;;
    --channel) CHANNEL="$2"; shift 2 ;;
    --seed) SEED="$2"; shift 2 ;;
    --promote) PROMOTE=1; shift ;;
    *) echo "publish-world: unknown argument $1" >&2; exit 1 ;;
  esac
done

for required in WORLD VERSION CHANNEL SEED; do
  if [ -z "${!required}" ]; then
    echo "publish-world: --$(echo "$required" | tr '[:upper:]' '[:lower:]') is required" >&2
    exit 1
  fi
done

FEED_TOOL="${FEED_TOOL:-cargo run -q -p lait-feed --}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PREFIX="releases/worlds/$WORLD/$VERSION"
POINTER_OBJECT="channels/worlds/$WORLD/$CHANNEL"

# A release coordinate is immutable. Re-running an identical publish is
# idempotent; attempting to put different bytes at an existing coordinate is
# a hard refusal. The generation-zero precondition closes the race between the
# public read and the upload.
upload_immutable() { # $1 local file, $2 object name below PREFIX
  local source="$1" object="$2" readback="$WORK/existing-$2"
  if curl -fsSL "$BASE_URL/$PREFIX/$object" -o "$readback"; then
    cmp "$source" "$readback" || {
      echo "publish-world: immutable $PREFIX/$object already exists with different bytes" >&2
      exit 1
    }
    echo "==> $object already published identically"
    return
  fi
  gcloud storage cp --if-generation-match=0 \
    --cache-control="public, max-age=31536000, immutable" \
    "$source" "$BUCKET/$PREFIX/$object"
}

if [ -n "$PROMOTE" ]; then
  # Promotion names a release already on the host. The sealed manifest is
  # fetched rather than rebuilt, so a promotion cannot quietly point at
  # different bytes than the ones testers have been running.
  echo "==> fetching the sealed manifest of $WORLD $VERSION"
  curl -fsSL "$BASE_URL/$PREFIX/manifest.json" -o "$WORK/manifest.json"
  # shellcheck disable=SC2086
  $FEED_TOOL world --world "$WORLD" --version "$VERSION" --channel "$CHANNEL" \
    --promote yes --base-url "$BASE_URL" --seed "$SEED" --out "$WORK"
else
  if [ "${#BUNDLES[@]}" -eq 0 ]; then
    echo "publish-world: at least one --bundle target=directory is required unless --promote" >&2
    exit 1
  fi
  FEED_BUNDLES=()
  ARCHIVES=()
  for spec in "${BUNDLES[@]}"; do
    target="${spec%%=*}"
    bundle="${spec#*=}"
    if [ "$target" = "$spec" ] || [ -z "$target" ] || [ -z "$bundle" ]; then
      echo "publish-world: --bundle must be target=directory, got $spec" >&2
      exit 1
    fi
    FEED_BUNDLES+=(--bundle "$spec")
    ARCHIVES+=("world-$WORLD-$VERSION-$target.tar.gz")
  done
  # shellcheck disable=SC2086
  $FEED_TOOL world --world "$WORLD" --version "$VERSION" \
    "${FEED_BUNDLES[@]}" --channel "$CHANNEL" --base-url "$BASE_URL" \
    --seed "$SEED" --out "$WORK"

  echo "==> uploading the native bundles"
  for archive in "${ARCHIVES[@]}"; do
    upload_immutable "$WORK/$archive" "$archive"
  done

  echo "==> uploading the signed manifest"
  upload_immutable "$WORK/manifest.json" "manifest.json"

  echo "==> reading every artifact back over the public door"
  for archive in "${ARCHIVES[@]}"; do
    curl -fsSL "$BASE_URL/$PREFIX/$archive" -o "$WORK/readback-$archive"
    cmp "$WORK/$archive" "$WORK/readback-$archive"
  done
  curl -fsSL "$BASE_URL/$PREFIX/manifest.json" -o "$WORK/readback.json"
  cmp "$WORK/manifest.json" "$WORK/readback.json"
fi

# The pointer moves last, no-cache, and only now: every object it names has
# been uploaded and read back over the door a follower will use.
echo "==> moving the $CHANNEL pointer for $WORLD"
gcloud storage cp --cache-control="no-cache, max-age=0" \
  "$WORK/pointer" "$BUCKET/$POINTER_OBJECT"

echo "==> reading the pointer back"
curl -fsSL "$BASE_URL/$POINTER_OBJECT" -o "$WORK/readback-pointer"
cmp "$WORK/pointer" "$WORK/readback-pointer"

echo "published $WORLD $VERSION to $CHANNEL"
