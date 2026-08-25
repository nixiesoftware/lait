#!/usr/bin/env bash
# Publish signed, independently installable first-party World fixtures for the
# process acceptance suite. The host test binary receives only this channel
# directory; it never discovers runner binaries beside itself or writes an
# installation record directly.
set -euo pipefail

OUT="${1:?usage: prepare-independent-world-fixtures.sh <empty-output-dir> <artifact-root> <lait-feed>}"
ARTIFACT_ROOT="${2:?usage: prepare-independent-world-fixtures.sh <empty-output-dir> <artifact-root> <lait-feed>}"
FEED_TOOL="${3:?usage: prepare-independent-world-fixtures.sh <empty-output-dir> <artifact-root> <lait-feed>}"

case "$OUT" in
  ""|"/"|".") echo "refusing unsafe fixture output: $OUT" >&2; exit 1 ;;
esac
[ -d "$ARTIFACT_ROOT" ] || {
  echo "independent World artifact root is absent: $ARTIFACT_ROOT" >&2
  exit 1
}
[ -x "$FEED_TOOL" ] || {
  echo "lait-feed is not executable: $FEED_TOOL" >&2
  exit 1
}
if [ -d "$OUT" ] && [ -n "$(find "$OUT" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
  echo "fixture output must be empty: $OUT" >&2
  exit 1
fi
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
ARTIFACT_ROOT="$(cd "$ARTIFACT_ROOT" && pwd)"
FEED_TOOL="$(cd "$(dirname "$FEED_TOOL")" && pwd)/$(basename "$FEED_TOOL")"
PUBLISH_STAGE="$(mktemp -d)"
trap 'rm -rf -- "$PUBLISH_STAGE"' EXIT

HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
[ -n "$HOST_TARGET" ] || {
  echo "rustc did not report its host target" >&2
  exit 1
}

PROFILE="${PROFILE:-debug}" ARTIFACT_ROOT="$ARTIFACT_ROOT" \
  bash .github/scripts/stage-worlds.sh "$HOST_TARGET" "$PUBLISH_STAGE"

KEY_OUTPUT="$OUT/keygen.out"
"$FEED_TOOL" keygen --out "$OUT/signing.seed" > "$KEY_OUTPUT"
sed -n 's/^public key (add to FEED_PUBKEYS_HEX): //p' "$KEY_OUTPUT" > "$OUT/pubkey.hex"
[ "$(wc -c < "$OUT/pubkey.hex")" -eq 65 ] || {
  echo "fixture publisher did not emit one 32-byte public key" >&2
  exit 1
}

version_of() {
  sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$1" | head -n 1
}

publish() {
  local world="$1" version="$2"
  local bundle="$PUBLISH_STAGE/worlds/$world/$version"
  local published="$OUT/$world"
  mkdir "$published"
  "$FEED_TOOL" world --world "$world" --version "$version" --channel test \
    --base-url "https://world-fixture.invalid" --seed "$OUT/signing.seed" \
    --bundle "$HOST_TARGET=$bundle" --out "$published"
}

publish "com.lait.issues" "$(version_of products/issues/Cargo.toml)"
publish "com.lait.signage" "$(version_of products/signage/Cargo.toml)"

# The private seed and publisher staging are not test inputs. Removing them
# makes it impossible for the acceptance process to manufacture another
# release after the signed channel has been handed over.
rm "$OUT/signing.seed" "$KEY_OUTPUT"

echo "signed independent World fixture channels: $OUT"
