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
# Canonical release path (the run id is printed by the Release workflow):
#   ci/publish-feed.sh --from-run 123456789 --version 0.9.1 --channel test \
#     --seed ~/.lait-feed-signing.seed [--floor 0.7.0]
#   ci/publish-feed.sh --version 0.9.1 --channel stable --promote \
#     --seed ~/.lait-feed-signing.seed
#
# `--from-run` downloads the complete native release artifact assembled by our
# Release workflow. GitHub Actions is transient build transport only; the
# signed GCS release and channel pointer created below are the distribution.
#
# Local/recovery path:
#   packaging/build-astrolabe.sh --version 0.9.1 --out target/distrib \
#     --identity "Developer ID Application: … (TEAMID)" --notarize <profile>
#   ci/publish-feed.sh --version 0.9.1 --channel test \
#     --artifacts-dir target/distrib --seed ~/.lait-feed-signing.seed
#
# Requires: gcloud (authenticated with write access to the bucket), curl, gh
# (for --from-run), and a built `lait-feed` (cargo build -p lait-feed).
#
# The seed never leaves the machine invoking this script. CI runs will replace
# the gcloud user credential with Workload Identity Federation (SUB-13, open);
# the sequencing here is identical either way.

set -euo pipefail

BUCKET="gs://the-foundation-dist"
BASE_URL="https://storage.googleapis.com/the-foundation-dist"

VERSION="" CHANNEL="" ARTIFACTS="" SEED="" FLOOR="" ASTROLABE="" FROM_RUN="" ARTIFACT_NAME="" LAIT_ONLY="" PROMOTE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --channel) CHANNEL="$2"; shift 2 ;;
    --artifacts-dir) ARTIFACTS="$2"; shift 2 ;;
    --seed) SEED="$2"; shift 2 ;;
    --floor) FLOOR="$2"; shift 2 ;;
    --astrolabe) ASTROLABE="$2"; shift 2 ;;
    --from-run) FROM_RUN="$2"; shift 2 ;;
    --artifact-name) ARTIFACT_NAME="$2"; shift 2 ;;
    --lait-only) LAIT_ONLY=1; shift ;;
    --promote) PROMOTE=1; shift ;;
    *) echo "publish-feed: unknown argument $1" >&2; exit 1 ;;
  esac
done

FEED_TOOL="${FEED_TOOL:-cargo run -q -p lait-feed --}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

RELEASE_PREFIX="releases/$VERSION"
POINTER_OBJECT="channels/$CHANNEL"

# A release coordinate is immutable. Re-running an identical publish is
# idempotent; attempting to put different bytes at an occupied coordinate is
# a hard refusal. The generation-zero precondition closes the race between the
# public read and the upload.
upload_immutable() { # $1 local file, $2 object name below RELEASE_PREFIX
  local source="$1" object="$2" readback="$WORK/existing-$(basename "$2")"
  if curl -fsSL "$BASE_URL/$RELEASE_PREFIX/$object" -o "$readback"; then
    cmp "$source" "$readback" || {
      echo "publish-feed: immutable $RELEASE_PREFIX/$object already exists with different bytes" >&2
      exit 1
    }
    echo "publish-feed: $object already published identically"
    return
  fi
  gcloud storage cp --if-generation-match=0 \
    --cache-control="public, max-age=31536000, immutable" \
    "$source" "$BUCKET/$RELEASE_PREFIX/$object"
}

[ -n "$VERSION" ] && [ -n "$CHANNEL" ] && [ -n "$SEED" ] || {
  echo "publish-feed: --version, --channel, and --seed are required" >&2
  exit 1
}

if [ -n "$PROMOTE" ]; then
  if [ -n "$FROM_RUN" ] || [ -n "$ARTIFACTS" ] || [ -n "$FLOOR" ] \
    || [ -n "$ASTROLABE" ] || [ -n "$ARTIFACT_NAME" ] || [ -n "$LAIT_ONLY" ]; then
    echo "publish-feed: --promote cannot rebuild or alter an immutable release" >&2
    exit 1
  fi
  # Promotion fetches the exact sealed manifest testers used. `pointer` opens
  # it with the publishing key and proves its version and compatibility floor
  # before it will sign the only mutable object.
  curl -fsSL "$BASE_URL/$RELEASE_PREFIX/manifest.json" -o "$WORK/manifest.json"
  # shellcheck disable=SC2086
  $FEED_TOOL pointer --channel "$CHANNEL" --version "$VERSION" \
    --manifest-url "$BASE_URL/$RELEASE_PREFIX/manifest.json" \
    --manifest "$WORK/manifest.json" --seed "$SEED" --out "$WORK/pointer.json"
  gcloud storage cp --cache-control="no-cache, max-age=0" \
    "$WORK/pointer.json" "$BUCKET/$POINTER_OBJECT"
  curl -fsSL "$BASE_URL/$POINTER_OBJECT" -o "$WORK/readback-pointer.json"
  cmp "$WORK/pointer.json" "$WORK/readback-pointer.json"
  echo "publish-feed: $CHANNEL now points at the tested $VERSION release"
  exit 0
fi

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

if [ -n "$FROM_RUN" ]; then
  [ -n "$VERSION" ] || {
    echo "publish-feed: --from-run also requires --version" >&2
    exit 1
  }
  ARTIFACTS="$WORK/run-artifacts"
  mkdir -p "$ARTIFACTS"
  [ -n "$ARTIFACT_NAME" ] || ARTIFACT_NAME="release-$VERSION"
  gh run download "$FROM_RUN" --repo nixiesoftware/lait \
    --name "$ARTIFACT_NAME" --dir "$ARTIFACTS"

  if [ -f "$ARTIFACTS/candidate-provenance.env" ]; then
    (cd "$ARTIFACTS" && sha256sum -c candidate-provenance.env.sha256)
    candidate_version="$(sed -n 's/^version=//p' "$ARTIFACTS/candidate-provenance.env")"
    source_sha="$(sed -n 's/^source_sha=//p' "$ARTIFACTS/candidate-provenance.env")"
    [ "$candidate_version" = "$VERSION" ] || {
      echo "publish-feed: candidate version $candidate_version does not match $VERSION" >&2
      exit 1
    }
    [[ "$source_sha" =~ ^[0-9a-f]{40}$ ]] || {
      echo "publish-feed: malformed candidate source SHA '$source_sha'" >&2
      exit 1
    }
    tag_sha="$(git rev-parse "refs/tags/v$VERSION^{commit}" 2>/dev/null || true)"
    [ "$tag_sha" = "$source_sha" ] || {
      echo "publish-feed: v$VERSION does not exist at audited candidate $source_sha" >&2
      exit 1
    }
    shopt -s nullglob
    attested=(
      "$ARTIFACTS"/candidate-provenance.env
      "$ARTIFACTS"/lait-*.zip
      "$ARTIFACTS"/lait-*.tar.gz
      "$ARTIFACTS"/astrolabe-*-setup.exe
      "$ARTIFACTS"/astrolabe-*.dmg
      "$ARTIFACTS"/astrolabe-*.tar.gz
    )
    for artifact in "${attested[@]}"; do
      gh attestation verify "$artifact" --repo nixiesoftware/lait \
        --source-digest "$source_sha" >/dev/null
    done
    echo "publish-feed: candidate provenance verified at $source_sha"
  elif [[ "$ARTIFACT_NAME" = candidate-* ]]; then
    echo "publish-feed: candidate artifact lacks candidate-provenance.env" >&2
    exit 1
  fi
  detect_astrolabe "Actions run $FROM_RUN"
elif [ -n "$ARTIFACT_NAME" ]; then
  echo "publish-feed: --artifact-name requires --from-run" >&2
  exit 1
fi

[ -n "$ARTIFACTS" ] || {
  echo "publish-feed: a new release requires --from-run or --artifacts-dir; use --promote for an existing release" >&2
  exit 1
}

# The local artifact path: build-astrolabe.sh dropped installers into the
# artifacts dir, and the pair rule makes their version $VERSION — so pin to
# it rather than trusting a listing over a directory nothing ever cleans,
# where `head -1` would happily publish LAST release's client. An installer
# for another version is refused by name, not skipped. An explicit
# --astrolabe still wins; a complete release run was already inspected above.
if [ -z "$ASTROLABE" ] && [ -z "$FROM_RUN" ]; then
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
for artifact in "$ARTIFACTS"/lait-*.zip "$ARTIFACTS"/lait-*.tar.gz; do
  upload_immutable "$artifact" "$(basename "$artifact")"
done
if [ -n "$ASTROLABE" ]; then
  # Whichever platform installers exist; the manifest step already refused an
  # $ASTROLABE with neither, and noted any absent platform loudly.
  for installer in "$ARTIFACTS/astrolabe-$ASTROLABE-setup.exe" \
                   "$ARTIFACTS/astrolabe-$ASTROLABE.dmg" \
                   "$ARTIFACTS/astrolabe-$ASTROLABE-x86_64-unknown-linux-gnu.tar.gz"; do
    [ -f "$installer" ] && upload_immutable "$installer" "$(basename "$installer")"
  done
  # The trees an updater consumes. The installers above are what a person
  # runs once; these are what every machine already running swaps in, and a
  # release that carries the first without the second can be installed but
  # never updated from.
  for tree in "$ARTIFACTS"/astrolabe-tree-"$ASTROLABE"-*.tar.gz; do
    [ -f "$tree" ] && upload_immutable "$tree" "$(basename "$tree")"
  done
fi
upload_immutable "$WORK/manifest.json" "manifest.json"

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
gcloud storage cp --cache-control="no-cache, max-age=0" \
  "$WORK/pointer.json" "$BUCKET/$POINTER_OBJECT"
curl -fsSL "$BASE_URL/$POINTER_OBJECT" -o "$WORK/readback-pointer.json"
cmp "$WORK/pointer.json" "$WORK/readback-pointer.json"

echo "publish-feed: $CHANNEL now points at $VERSION"
