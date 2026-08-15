#!/usr/bin/env bash
#
# publish-feed.sh — push one release through the first-party feed (SUB-13).
#
# The feed's one law of motion: the channel pointer is the only mutable object,
# and it moves LAST. Every artifact and the signed manifest are uploaded and
# read back before the pointer that names them is rewritten — a pointer naming
# missing artifacts is a broken feed for every follower at once. The rules that
# must fail a publish (an unsatisfiable floor, a stable pointer naming a
# prerelease) live in `lait-feed`, which refuses to seal the pointer; this
# script only sequences uploads.
#
# Usage:
#   ci/publish-feed.sh --version 0.8.0-test.1 --channel test \
#     --artifacts-dir target/distrib --seed ~/.lait-feed-signing.seed \
#     [--floor 0.7.0] [--astrolabe 0.1.0]
#
#   ci/publish-feed.sh --from-release v0.8.0-test.1 --channel test \
#     --seed ~/.lait-feed-signing.seed [--floor 0.7.0]
#
# `--from-release` is the seamless path: it downloads the tag's CI-built
# artifacts from the GitHub release (the lait archives, and the Astrolabe
# installer when the Build Astrolabe installer workflow has attached it),
# derives the versions, and publishes exactly as the explicit form does. The
# release is the build origin; the feed is what installed machines read.
#
# Requires: gcloud (authenticated with write access to the bucket), curl, gh
# (for --from-release), and a built `lait-feed` (cargo build -p lait-feed).
#
# The seed never leaves the machine invoking this script. CI runs will replace
# the gcloud user credential with Workload Identity Federation (SUB-13, open);
# the sequencing here is identical either way.

set -euo pipefail

BUCKET="gs://the-foundation-dist"
BASE_URL="https://storage.googleapis.com/the-foundation-dist"

VERSION="" CHANNEL="" ARTIFACTS="" SEED="" FLOOR="" ASTROLABE="" FROM_RELEASE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --channel) CHANNEL="$2"; shift 2 ;;
    --artifacts-dir) ARTIFACTS="$2"; shift 2 ;;
    --seed) SEED="$2"; shift 2 ;;
    --floor) FLOOR="$2"; shift 2 ;;
    --astrolabe) ASTROLABE="$2"; shift 2 ;;
    --from-release) FROM_RELEASE="$2"; shift 2 ;;
    *) echo "publish-feed: unknown argument $1" >&2; exit 1 ;;
  esac
done

FEED_TOOL="${FEED_TOOL:-cargo run -q -p lait-feed --}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

if [ -n "$FROM_RELEASE" ]; then
  VERSION="${FROM_RELEASE#v}"
  ARTIFACTS="$WORK/release-assets"
  mkdir -p "$ARTIFACTS"
  gh release download "$FROM_RELEASE" -D "$ARTIFACTS" \
    -p 'lait-*.zip' -p 'lait-*.tar.gz' -p 'astrolabe-*-setup.exe' -p 'astrolabe-*.dmg'
  # The installers name their own bundle version; read it off an asset rather
  # than assuming — an absent installer publishes a lait-only release, loudly.
  # Windows setup.exe and macOS dmg carry the same version by construction
  # (one gate job resolves the tag both build from), so either may name it.
  installer="$(cd "$ARTIFACTS" && ls astrolabe-*-setup.exe astrolabe-*.dmg 2>/dev/null | head -1 || true)"
  if [ -n "$installer" ]; then
    ASTROLABE="${installer#astrolabe-}"
    ASTROLABE="${ASTROLABE%-setup.exe}"; ASTROLABE="${ASTROLABE%.dmg}"
    echo "publish-feed: including installer(s) for astrolabe $ASTROLABE"
  else
    echo "publish-feed: NOTE — no astrolabe installer on $FROM_RELEASE; publishing lait only" >&2
  fi
fi

[ -n "$VERSION" ] && [ -n "$CHANNEL" ] && [ -n "$ARTIFACTS" ] && [ -n "$SEED" ] || {
  echo "publish-feed: --channel and --seed, plus either --from-release or --version + --artifacts-dir, are required" >&2
  exit 1
}

# 1. Seal the manifest from the artifacts as they exist on disk. Refuses a
#    directory missing any lait target.
MANIFEST_ARGS=(--version "$VERSION" --base-url "$BASE_URL" \
  --artifacts-dir "$ARTIFACTS" --seed "$SEED" --out "$WORK/manifest.json")
[ -n "$FLOOR" ] && MANIFEST_ARGS+=(--floor "$FLOOR")
[ -n "$ASTROLABE" ] && MANIFEST_ARGS+=(--astrolabe "$ASTROLABE")
$FEED_TOOL manifest "${MANIFEST_ARGS[@]}"

# 2. Seal the pointer — the step that enforces the floor and prerelease rules
#    against the manifest it will name, BEFORE anything is uploaded.
$FEED_TOOL pointer --channel "$CHANNEL" --version "$VERSION" \
  --manifest-url "$BASE_URL/releases/$VERSION/manifest.json" \
  --manifest "$WORK/manifest.json" --seed "$SEED" --out "$WORK/pointer.json"

# 3. Upload the immutable release: artifacts first, manifest with them, all
#    long-cache — a version's objects never change after publish.
IMMUTABLE="public, max-age=31536000, immutable"
gcloud storage cp --cache-control="$IMMUTABLE" \
  "$ARTIFACTS"/lait-*.zip "$ARTIFACTS"/lait-*.tar.gz \
  "$BUCKET/releases/$VERSION/"
if [ -n "$ASTROLABE" ]; then
  # Whichever platform installers exist; the manifest step already refused an
  # $ASTROLABE with neither, and noted any absent platform loudly.
  for installer in "$ARTIFACTS/astrolabe-$ASTROLABE-setup.exe" \
                   "$ARTIFACTS/astrolabe-$ASTROLABE.dmg"; do
    [ -f "$installer" ] && gcloud storage cp --cache-control="$IMMUTABLE" \
      "$installer" "$BUCKET/releases/$VERSION/"
  done
fi
gcloud storage cp --cache-control="$IMMUTABLE" \
  "$WORK/manifest.json" "$BUCKET/releases/$VERSION/manifest.json"

# 4. Read the release back over the same door installed machines use, before
#    the pointer moves. An upload that "succeeded" but does not serve is
#    exactly the failure this ordering exists to keep out of the feed.
for object in $(cd "$ARTIFACTS" && ls lait-*.zip lait-*.tar.gz astrolabe-*-setup.exe astrolabe-*.dmg 2>/dev/null) manifest.json; do
  curl -fsSLo /dev/null "$BASE_URL/releases/$VERSION/$object" \
    || { echo "publish-feed: $object uploaded but not served; pointer NOT moved" >&2; exit 1; }
done

# 5. Only now: move the pointer. no-cache, because this is the one object a
#    long-lived node re-reads.
gcloud storage cp --cache-control="no-cache" \
  "$WORK/pointer.json" "$BUCKET/channels/$CHANNEL"

echo "publish-feed: $CHANNEL now points at $VERSION"
