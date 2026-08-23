#!/usr/bin/env bash
#
# make-tree.sh — pack the swap-consumable release tree (CLIENT-65).
#
# The installers produce what a *person* runs: an NSIS setup, a signed DMG, a
# tarball. None of those can be extracted portably by an updater, so a release
# also carries a tree artifact: the exact contents the stub swaps into
# `current/`, as a gzip'd tar with one root directory.
#
# The shape differs per platform because the unit of replacement does:
#
#   windows / linux   the pair and the engine payload, flat under the root,
#                     which is what lands in `current/`
#   macos             the .app's own contents under the root, because there
#                     the bundle is the unit and `bundle::exchange` replaces
#                     it whole
#
# Deterministic — sorted entries, zeroed mtimes and ownership — so republishing
# an unchanged build produces an unchanged digest, and a digest that moved
# means the content moved.
#
# Usage:
#   packaging/make-tree.sh --stage <dir> --version <x.y.z> \
#     --target <triple> --out <dir>
#
# where --stage is the Tauri release tree (Windows, Linux) or the .app
# (macos).

set -euo pipefail

STAGE="" VERSION="" TARGET="" OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --stage)   STAGE="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --target)  TARGET="$2"; shift 2 ;;
    --out)     OUT="$2"; shift 2 ;;
    *) echo "make-tree: unknown argument $1" >&2; exit 1 ;;
  esac
done

[ -n "$STAGE" ] && [ -n "$VERSION" ] && [ -n "$TARGET" ] && [ -n "$OUT" ] || {
  echo "make-tree: --stage, --version, --target and --out are required" >&2
  exit 1
}
[ -d "$STAGE" ] || { echo "make-tree: --stage $STAGE is not a directory" >&2; exit 1; }

# The pair must be present and positioned as the platform expects, because a
# tree missing either half installs a client that cannot find its daemon —
# the machine `sidecar::beside` and `update::custody_of` exist to prevent, and
# the staging half refuses it too. Refused here so a release never carries it.
#
# The terms and the notices are required for the same reason, one layer up: the
# tree produced here is what a self-update swaps into `current/`, so a file the
# *installer* adds afterwards survives the install and not the first upgrade.
# That is not hypothetical — Linux staged the notices only in make-tarball,
# which writes to the tarball's own root, so its update tree carried none and
# every Linux user lost them on their first self-update. Refusing here puts the
# check in the script that makes the artifact, rather than in a test that reads
# a CI workflow and cannot see a local or future invocation at all.
case "$TARGET" in
  *-windows-*) ENTRY="astrolabe.exe";            SIDECAR="lait.exe"
               DOCS="." ;;
  *-apple-*)   ENTRY="Contents/MacOS/astrolabe"; SIDECAR="Contents/MacOS/lait"
               DOCS="Contents/Resources" ;;
  *)           ENTRY="astrolabe";                SIDECAR="lait"
               DOCS="." ;;
esac
for required in "$ENTRY" "$SIDECAR"; do
  [ -f "$STAGE/$required" ] || {
    echo "make-tree: $STAGE is missing $required — a tree without both halves of the pair" >&2
    exit 1
  }
done
for required in "$DOCS/LICENSE" "$DOCS/THIRD-PARTY-NOTICES.md"; do
  [ -f "$STAGE/$required" ] || {
    echo "make-tree: $STAGE is missing $required — a tree that drops the terms on the first self-update" >&2
    exit 1
  }
done

mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
STAGE="$(cd "$STAGE" && pwd)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
ROOT_NAME="astrolabe-tree-$VERSION-$TARGET"
STAGED="$WORK/$ROOT_NAME"
mkdir "$STAGED"

# The directory is the unit; enumerating today's files would make tomorrow's
# newly required one the file an old script drops.
cp -a "$STAGE/." "$STAGED/"

find "$STAGED" -type d -exec chmod 0755 {} +
find "$STAGED" -type f -exec chmod 0644 {} +
chmod 0755 "$STAGED/$ENTRY" "$STAGED/$SIDECAR"

# Determinism, portably. GNU tar has --sort and --mtime; bsdtar (macOS, where
# the .app tree is packed) has neither, so the two properties are established
# on the filesystem instead: every timestamp zeroed with `touch`, and the
# member order fixed by handing tar an already-sorted list. Ownership is
# normalised with whichever flag spelling the local tar understands.
find "$STAGED" -exec touch -t 197001010000 {} +
if tar --version 2>&1 | head -1 | grep -qi bsdtar; then
  OWNERSHIP=(--uid 0 --gid 0 --uname "" --gname "")
else
  OWNERSHIP=(--owner=0 --group=0 --numeric-owner)
fi

ARCHIVE="$OUT/$ROOT_NAME.tar.gz"
(cd "$WORK" && find "$ROOT_NAME" | LC_ALL=C sort | \
  tar "${OWNERSHIP[@]}" --no-recursion -T - -czf "$ARCHIVE")

if command -v sha256sum >/dev/null 2>&1; then
  (cd "$OUT" && sha256sum "$ROOT_NAME.tar.gz" > "$ROOT_NAME.tar.gz.sha256")
else
  (cd "$OUT" && shasum -a 256 "$ROOT_NAME.tar.gz" > "$ROOT_NAME.tar.gz.sha256")
fi

echo "packed $ARCHIVE"
