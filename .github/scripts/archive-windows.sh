#!/usr/bin/env bash
# Build the Windows release archive for one target, matching cargo-dist's layout:
# a FLAT `.zip` (lait.exe + the misc docs at the archive root, no nesting dir),
# plus a `<archive>.sha256` sidecar. Runs under Git Bash on windows runners.
#
# Usage: archive-windows.sh <target-triple>   (run from the repo root)
set -euo pipefail

TARGET="${1:?usage: archive-windows.sh <target-triple>}"
NAME="lait-${TARGET}"
BIN="target/${TARGET}/release/lait.exe"
ARCHIVE="${NAME}.zip"
STAGE="target/archive-windows/${NAME}"

[ -f "$BIN" ] || { echo "::error::binary not found: $BIN"; exit 1; }

rm -rf "$ARCHIVE" "${ARCHIVE}.sha256" "$STAGE"
mkdir -p "$STAGE"
trap 'rm -rf "$STAGE"' EXIT
# Flat archive: binary + misc docs at the zip root (matches the published layout).
cp "$BIN" "$STAGE/lait.exe"
cp CHANGELOG.md LICENSE README.md "$STAGE/"
bash .github/scripts/stage-worlds.sh "$TARGET" "$STAGE"
(
  cd "$STAGE"
  7z a "../../../$ARCHIVE" lait.exe worlds CHANGELOG.md LICENSE README.md >/dev/null
)
sha256sum "$ARCHIVE" > "${ARCHIVE}.sha256"

echo "built $ARCHIVE"
7z l "$ARCHIVE"
