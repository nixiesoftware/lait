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
# Canonical release path (the run id and artifact name are printed by the
# World Candidate workflow, which builds, checksums, and attests every native
# bundle and moves no channel):
#   ci/publish-world.sh --world com.lait.issues --version 0.1.0 \
#     --from-run 123456789 --artifact-name world-candidate-abcdef123456 \
#     --channel test --seed ~/.lait-feed-signing.seed
#
#   ci/publish-world.sh --world com.lait.issues --version 0.1.0 \
#     --channel stable --promote --seed ~/.lait-feed-signing.seed
#
# `--from-run` downloads the complete audited candidate, refuses it unless its
# recorded source SHA, checksums, provenance attestations, and signing
# workflow identity all verify, and publishes exactly those bytes. GitHub
# Actions is transient build transport only; the signed GCS release and
# channel pointer created below are the distribution.
#
# Local/recovery path:
#   ci/publish-world.sh --world com.lait.issues --version 0.1.0 \
#     --bundle x86_64-unknown-linux-gnu=target/linux/issues \
#     --bundle x86_64-pc-windows-msvc=target/windows/issues \
#     --channel test --seed ~/.lait-feed-signing.seed
#
# The bundle must carry `world.json` at its root: what the World is, how to
# reach it, and the host facts it runs against. `lait-feed world` refuses
# without one, so a publisher learns at publish time rather than a machine
# learning after it has already downloaded.
#
# Requires: gcloud (authenticated with write access to the bucket), curl, gh
# (for --from-run), and a built `lait-feed` (cargo build -p lait-feed).
#
# The seed never leaves the machine invoking this script.

set -euo pipefail

BUCKET="gs://the-foundation-dist"
BASE_URL="https://storage.googleapis.com/the-foundation-dist"

WORLD="" VERSION="" CHANNEL="" SEED="" PROMOTE="" FROM_RUN="" ARTIFACT_NAME=""
BUNDLES=()
while [ $# -gt 0 ]; do
  case "$1" in
    --world) WORLD="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --bundle) BUNDLES+=("$2"); shift 2 ;;
    --channel) CHANNEL="$2"; shift 2 ;;
    --seed) SEED="$2"; shift 2 ;;
    --from-run) FROM_RUN="$2"; shift 2 ;;
    --artifact-name) ARTIFACT_NAME="$2"; shift 2 ;;
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

if [ -n "$PROMOTE" ] && { [ -n "$FROM_RUN" ] || [ -n "$ARTIFACT_NAME" ] \
  || [ "${#BUNDLES[@]}" -gt 0 ]; }; then
  echo "publish-world: --promote cannot rebuild or alter an immutable release" >&2
  exit 1
fi
if [ -n "$ARTIFACT_NAME" ] && [ -z "$FROM_RUN" ]; then
  echo "publish-world: --artifact-name requires --from-run" >&2
  exit 1
fi

FEED_TOOL="${FEED_TOOL:-cargo run -q -p lait-feed --}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

if [ -n "$FROM_RUN" ]; then
  if [ "${#BUNDLES[@]}" -gt 0 ]; then
    echo "publish-world: --from-run supplies every native bundle; --bundle contradicts it" >&2
    exit 1
  fi
  # The candidate artifact is commit-addressed, not version-addressed, so its
  # name cannot be derived here: take it from the candidate run's summary.
  [ -n "$ARTIFACT_NAME" ] || {
    echo "publish-world: --from-run requires --artifact-name world-candidate-<short-sha>" >&2
    exit 1
  }
  CANDIDATE="$WORK/candidate"
  mkdir -p "$CANDIDATE"
  gh run download "$FROM_RUN" --repo nixiesoftware/lait \
    --name "$ARTIFACT_NAME" --dir "$CANDIDATE"

  # The candidate's own coordinate first: the recorded source SHA is what
  # every attestation below must agree with, and the env's attestation binds
  # the record itself to the workflow run that assembled the bytes.
  [ -f "$CANDIDATE/world-candidate-provenance.env" ] || {
    echo "publish-world: candidate artifact lacks world-candidate-provenance.env" >&2
    exit 1
  }
  (cd "$CANDIDATE" && sha256sum -c world-candidate-provenance.env.sha256 >/dev/null)
  source_sha="$(sed -n 's/^source_sha=//p' "$CANDIDATE/world-candidate-provenance.env")"
  [[ "$source_sha" =~ ^[0-9a-f]{40}$ ]] || {
    echo "publish-world: malformed candidate source SHA '$source_sha'" >&2
    exit 1
  }
  case "$WORLD" in
    com.lait.issues)
      candidate_version="$(sed -n 's/^issues_version=//p' "$CANDIDATE/world-candidate-provenance.env")" ;;
    com.lait.signage)
      candidate_version="$(sed -n 's/^signage_version=//p' "$CANDIDATE/world-candidate-provenance.env")" ;;
    *)
      echo "publish-world: the candidate records no version for $WORLD" >&2
      exit 1 ;;
  esac
  [ "$candidate_version" = "$VERSION" ] || {
    echo "publish-world: the candidate built $WORLD $candidate_version, not $VERSION" >&2
    exit 1
  }

  # Provenance is three claims and all must hold for every file published:
  # built from this exact source commit, in our repository, by the World
  # Candidate workflow — not any workflow that happens to run in our name.
  SIGNER_WORKFLOW="nixiesoftware/lait/.github/workflows/publish-worlds.yml"
  gh attestation verify "$CANDIDATE/world-candidate-provenance.env" \
    --repo nixiesoftware/lait --source-digest "$source_sha" \
    --signer-workflow "$SIGNER_WORKFLOW" >/dev/null
  CANDIDATE_TARGETS=(
    x86_64-unknown-linux-gnu
    aarch64-unknown-linux-gnu
    aarch64-apple-darwin
    x86_64-apple-darwin
    x86_64-pc-windows-msvc
  )
  for target in "${CANDIDATE_TARGETS[@]}"; do
    archive="$CANDIDATE/world-bundles-$target.tar.gz"
    [ -f "$archive" ] || {
      echo "publish-world: candidate lacks world-bundles-$target.tar.gz" >&2
      exit 1
    }
    (cd "$CANDIDATE" && sha256sum -c "world-bundles-$target.tar.gz.sha256" >/dev/null)
    gh attestation verify "$archive" \
      --repo nixiesoftware/lait --source-digest "$source_sha" \
      --signer-workflow "$SIGNER_WORKFLOW" >/dev/null
    mkdir -p "$CANDIDATE/$target"
    tar xzf "$archive" -C "$CANDIDATE/$target"
    bundle="$CANDIDATE/$target/worlds/$WORLD/$VERSION"
    [ -d "$bundle" ] || {
      echo "publish-world: the $target candidate tree carries no $WORLD $VERSION" >&2
      exit 1
    }
    BUNDLES+=("$target=$bundle")
  done
  echo "==> candidate provenance verified at $source_sha"
fi

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
    echo "publish-world: a new release requires --from-run or --bundle target=directory; use --promote for an existing release" >&2
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

# Say so. The bucket is the authority and every machine's period is the floor;
# the notify relay is what makes the floor irrelevant. The body is the pointer
# just published — the relay verifies it against the feed key, so there is
# nothing to authenticate here. Non-fatal: a relay that is down costs latency,
# never the publish.
NOTIFY_URL="${LAIT_FEED_NOTIFY-https://foundation-notify-894246603476.us-central1.run.app}"
if [ -n "$NOTIFY_URL" ]; then
  if curl -fsS -X POST -H 'Content-Type: application/json' \
      --data-binary "@$WORK/pointer" \
      "$NOTIFY_URL/announce/${POINTER_OBJECT#channels/}" -o /dev/null; then
    echo "publish-world: announced $WORLD $CHANNEL to $NOTIFY_URL"
  else
    echo "publish-world: WARNING: $NOTIFY_URL did not take the announcement; machines learn on their period" >&2
  fi
fi
