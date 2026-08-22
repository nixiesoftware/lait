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
# **The feed is how the client is distributed.** Installed machines follow a
# signed channel pointer on the dist host; nothing they do involves a git
# forge. This is the publish half of that, and `packaging/build-astrolabe.sh`
# is the build half — it emits exactly the names read below.
#
# Usage (the path to use):
#   packaging/build-astrolabe.sh --version 0.9.0 --out target/distrib \
#     --identity "Developer ID Application: … (TEAMID)" --notarize <profile>
#   ci/publish-feed.sh --version 0.9.0 --channel test \
#     --artifacts-dir target/distrib --seed ~/.lait-feed-signing.seed \
#     [--floor 0.7.0] [--astrolabe 0.9.0]   (--astrolabe is read off the
#     installers in --artifacts-dir when omitted; --lait-only skips the client)
#
# DEPRECATED path:
#   ci/publish-feed.sh --from-release v0.8.0-test.1 --channel test \
#     --seed ~/.lait-feed-signing.seed [--floor 0.7.0]
#
# `--from-release` downloads a tag's artifacts from a GitHub release. It is
# kept for republishing an already-released tag, and it is not how anything is
# shipped now: the workflow that attached Astrolabe installers to releases
# built the deprecated Flutter client and is itself unwired
# (`apps/astrolabe/DEPRECATED.md`), so on any recent tag this path finds lait
# archives and no client, and refuses unless you pass --lait-only. Build
# locally and pass --artifacts-dir instead.
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

VERSION="" CHANNEL="" ARTIFACTS="" SEED="" FLOOR="" ASTROLABE="" FROM_RELEASE="" LAIT_ONLY=""
while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --channel) CHANNEL="$2"; shift 2 ;;
    --artifacts-dir) ARTIFACTS="$2"; shift 2 ;;
    --seed) SEED="$2"; shift 2 ;;
    --floor) FLOOR="$2"; shift 2 ;;
    --astrolabe) ASTROLABE="$2"; shift 2 ;;
    --from-release) FROM_RELEASE="$2"; shift 2 ;;
    --lait-only) LAIT_ONLY=1; shift ;;
    *) echo "publish-feed: unknown argument $1" >&2; exit 1 ;;
  esac
done

FEED_TOOL="${FEED_TOOL:-cargo run -q -p lait-feed --}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# The installers name their own bundle version; read it off an asset rather
# than assuming — an absent installer publishes a lait-only release, loudly,
# and only when --lait-only says so. Every platform artifact carries the same
# version by construction (one gate resolves the tag all jobs build), so any
# one may name it. Trees are excluded: a tree is not an installer, and the
# tarball glob would otherwise read a version out of `astrolabe-tree-…`.
detect_astrolabe() { # $1 = where the artifacts came from, for the refusal
  if [ -n "$LAIT_ONLY" ]; then
    echo "publish-feed: publishing lait only, as asked; any client artifacts in $1 stay unpublished" >&2
    return
  fi
  local installer
  installer="$(cd "$ARTIFACTS" && \
    ls astrolabe-*-setup.exe astrolabe-*.dmg astrolabe-*.tar.gz 2>/dev/null \
    | grep -v '^astrolabe-tree-' | head -1 || true)"
  if [ -n "$installer" ]; then
    ASTROLABE="${installer#astrolabe-}"
    case "$ASTROLABE" in
      *-setup.exe) ASTROLABE="${ASTROLABE%-setup.exe}" ;;
      *.dmg) ASTROLABE="${ASTROLABE%.dmg}" ;;
      *-x86_64-unknown-linux-gnu.tar.gz)
        ASTROLABE="${ASTROLABE%-x86_64-unknown-linux-gnu.tar.gz}" ;;
      *) echo "publish-feed: unrecognized Astrolabe artifact $installer" >&2; exit 1 ;;
    esac
    echo "publish-feed: including installer(s) for astrolabe $ASTROLABE"
  else
    echo "publish-feed: $1 carries no astrolabe installer." >&2
    echo "  A release that ships the engine and not the client is a real thing to want —" >&2
    echo "  lait is installed on its own by several paths — but it is not a thing to do by" >&2
    echo "  ACCIDENT, which is what a note on stderr and a publish anyway amounts to." >&2
    echo "  Pass --lait-only to say you meant it." >&2
    exit 1
  fi
}

if [ -n "$FROM_RELEASE" ]; then
  VERSION="${FROM_RELEASE#v}"
  ARTIFACTS="$WORK/release-assets"
  mkdir -p "$ARTIFACTS"
  gh release download "$FROM_RELEASE" -D "$ARTIFACTS" \
    -p 'lait-*.zip' -p 'lait-*.tar.gz' -p 'astrolabe-*-setup.exe' \
    -p 'astrolabe-tree-*.tar.gz' \
    -p 'astrolabe-*.dmg' -p 'astrolabe-*.tar.gz'
  detect_astrolabe "$FROM_RELEASE"

  # The stable pointer never moves at a release GitHub still calls a prerelease.
  # The release page is the record of whether every declared bundle actually
  # arrived; disagreeing with it here would make the feed the more optimistic of
  # two accounts of the same release.
  if [ "$CHANNEL" = "stable" ]; then
    prerelease="$(gh release view "$FROM_RELEASE" --json isPrerelease --jq .isPrerelease 2>/dev/null || echo unknown)"
    if [ "$prerelease" != "false" ]; then
      echo "publish-feed: $FROM_RELEASE is marked isPrerelease=$prerelease on GitHub." >&2
      echo "  The stable pointer is what every installed machine follows by default." >&2
      echo "  Promote the release first, or publish to --channel test." >&2
      exit 1
    fi
  fi
fi

[ -n "$VERSION" ] && [ -n "$CHANNEL" ] && [ -n "$ARTIFACTS" ] && [ -n "$SEED" ] || {
  echo "publish-feed: --channel and --seed, plus either --from-release or --version + --artifacts-dir, are required" >&2
  exit 1
}

# The primary documented path: build-astrolabe.sh dropped installers into the
# artifacts dir, and the pair rule makes their version $VERSION — so pin to
# it rather than trusting a listing over a directory nothing ever cleans,
# where `head -1` would happily publish LAST release's client. An installer
# for another version is refused by name, not skipped. An explicit
# --astrolabe still wins; --from-release keeps listing-detection because old
# tags predate the pair rule and legitimately carry another client version.
if [ -z "$ASTROLABE" ] && [ -z "$FROM_RELEASE" ]; then
  if [ -n "$LAIT_ONLY" ]; then
    detect_astrolabe "$ARTIFACTS"
  elif [ -f "$ARTIFACTS/astrolabe-$VERSION-setup.exe" ] \
    || [ -f "$ARTIFACTS/astrolabe-$VERSION.dmg" ] \
    || [ -f "$ARTIFACTS/astrolabe-$VERSION-x86_64-unknown-linux-gnu.tar.gz" ]; then
    ASTROLABE="$VERSION"
    echo "publish-feed: including installer(s) for astrolabe $ASTROLABE"
  else
    stale="$(cd "$ARTIFACTS" && \
      ls astrolabe-*-setup.exe astrolabe-*.dmg astrolabe-*.tar.gz 2>/dev/null \
      | grep -v '^astrolabe-tree-' | head -1 || true)"
    if [ -n "$stale" ]; then
      echo "publish-feed: $ARTIFACTS holds $stale but no installer for $VERSION." >&2
      echo "  A stale artifacts dir is how last release's client ships under this" >&2
      echo "  release's manifest. Rebuild with build-astrolabe.sh --version $VERSION," >&2
      echo "  clean the directory, or pass --astrolabe to name the version you mean." >&2
      exit 1
    fi
    detect_astrolabe "$ARTIFACTS"
  fi
elif [ -n "$ASTROLABE" ] && [ -n "$LAIT_ONLY" ]; then
  echo "publish-feed: --astrolabe and --lait-only contradict each other" >&2
  exit 1
fi

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
                   "$ARTIFACTS/astrolabe-$ASTROLABE.dmg" \
                   "$ARTIFACTS/astrolabe-$ASTROLABE-x86_64-unknown-linux-gnu.tar.gz"; do
    [ -f "$installer" ] && gcloud storage cp --cache-control="$IMMUTABLE" \
      "$installer" "$BUCKET/releases/$VERSION/"
  done
  # The trees an updater consumes. The installers above are what a person
  # runs once; these are what every machine already running swaps in, and a
  # release that carries the first without the second can be installed but
  # never updated from.
  for tree in "$ARTIFACTS"/astrolabe-tree-"$ASTROLABE"-*.tar.gz; do
    [ -f "$tree" ] && gcloud storage cp --cache-control="$IMMUTABLE" \
      "$tree" "$BUCKET/releases/$VERSION/"
  done
fi
gcloud storage cp --cache-control="$IMMUTABLE" \
  "$WORK/manifest.json" "$BUCKET/releases/$VERSION/manifest.json"

# 4. Read the release back over the same door installed machines use, before
#    the pointer moves. An upload that "succeeded" but does not serve is
#    exactly the failure this ordering exists to keep out of the feed.
#    The list mirrors what step 3 uploaded, never the directory's contents: a
#    local file that was deliberately not published — another version's
#    installer in a reused directory included — must not fail the publish.
VERIFY="manifest.json $(cd "$ARTIFACTS" && ls lait-*.zip lait-*.tar.gz 2>/dev/null || true)"
if [ -n "$ASTROLABE" ]; then
  for uploaded in "astrolabe-$ASTROLABE-setup.exe" "astrolabe-$ASTROLABE.dmg" \
    "astrolabe-$ASTROLABE-x86_64-unknown-linux-gnu.tar.gz"; do
    [ -f "$ARTIFACTS/$uploaded" ] && VERIFY="$VERIFY $uploaded"
  done
  VERIFY="$VERIFY $(cd "$ARTIFACTS" && \
    ls astrolabe-tree-"$ASTROLABE"-*.tar.gz 2>/dev/null || true)"
fi
for object in $VERIFY; do
  curl -fsSLo /dev/null "$BASE_URL/releases/$VERSION/$object" \
    || { echo "publish-feed: $object uploaded but not served; pointer NOT moved" >&2; exit 1; }
done

# 5. Only now: move the pointer. no-cache, because this is the one object a
#    long-lived node re-reads.
gcloud storage cp --cache-control="no-cache" \
  "$WORK/pointer.json" "$BUCKET/channels/$CHANNEL"

echo "publish-feed: $CHANNEL now points at $VERSION"
