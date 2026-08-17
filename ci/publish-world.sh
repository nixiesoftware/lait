#!/usr/bin/env bash
#
# publish-world.sh — ship a World's web head in one act (SUB-23).
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
#     --runtime <runtime-token> --bundle viewer/dist --channel test \
#     --seed ~/.lait-feed-signing.seed
#
#   ci/publish-world.sh --world com.lait.issues --version 0.1.0 \
#     --channel stable --promote --seed ~/.lait-feed-signing.seed
#
# Requires: gcloud (authenticated with write access to the bucket), curl, and
# a built `lait-feed` (cargo build -p lait-feed).
#
# The seed never leaves the machine invoking this script.

set -euo pipefail

BUCKET="gs://the-foundation-dist"
BASE_URL="https://storage.googleapis.com/the-foundation-dist"

WORLD="" VERSION="" RUNTIME="" BUNDLE="" CHANNEL="" SEED="" PROMOTE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --world) WORLD="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --runtime) RUNTIME="$2"; shift 2 ;;
    --bundle) BUNDLE="$2"; shift 2 ;;
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
  if [ -z "$RUNTIME" ] || [ -z "$BUNDLE" ]; then
    echo "publish-world: --runtime and --bundle are required unless --promote" >&2
    exit 1
  fi
  # shellcheck disable=SC2086
  $FEED_TOOL world --world "$WORLD" --version "$VERSION" --runtime "$RUNTIME" \
    --bundle "$BUNDLE" --channel "$CHANNEL" --base-url "$BASE_URL" \
    --seed "$SEED" --out "$WORK"

  ARCHIVE="world-$WORLD-$VERSION.tar.gz"
  echo "==> uploading the bundle"
  gcloud storage cp --cache-control="public, max-age=31536000, immutable" \
    "$WORK/$ARCHIVE" "$BUCKET/$PREFIX/$ARCHIVE"

  echo "==> uploading the signed manifest"
  gcloud storage cp --cache-control="public, max-age=31536000, immutable" \
    "$WORK/manifest.json" "$BUCKET/$PREFIX/manifest.json"

  echo "==> reading both back over the public door"
  curl -fsSL "$BASE_URL/$PREFIX/$ARCHIVE" -o "$WORK/readback.tar.gz"
  cmp "$WORK/$ARCHIVE" "$WORK/readback.tar.gz"
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
