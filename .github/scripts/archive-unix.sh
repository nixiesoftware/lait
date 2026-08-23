#!/usr/bin/env bash
# Build the canonical feed archive for one Unix target: a `.tar.gz` whose
# contents are nested under `lait-<target>/`, plus a `.sha256` sidecar.
#
# Usage: archive-unix.sh <target-triple>   (run from the repo root)
set -euo pipefail

TARGET="${1:?usage: archive-unix.sh <target-triple>}"
NAME="lait-${TARGET}"
BIN="target/${TARGET}/release/lait"
ARCHIVE="${NAME}.tar.gz"

[ -f "$BIN" ] || { echo "::error::binary not found: $BIN"; exit 1; }

rm -rf "$NAME" "$ARCHIVE" "${ARCHIVE}.sha256"
mkdir -p "$NAME"
cp "$BIN" "$NAME/lait"
# Keep this documented payload in sync with the release contract.
cp CHANGELOG.md LICENSE README.md "$NAME/"
bash .github/scripts/stage-worlds.sh "$TARGET" "$NAME"

tar czf "$ARCHIVE" "$NAME"
shasum -a 256 "$ARCHIVE" > "${ARCHIVE}.sha256"

echo "built $ARCHIVE"
tar tzf "$ARCHIVE"
