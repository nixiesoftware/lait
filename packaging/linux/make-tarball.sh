#!/usr/bin/env bash
#
# Astrolabe — relocatable Linux desktop bundle.
#
# The Tauri host and lait travel as a pair beside their terms. This script
# refuses an incomplete stage, then seals it under the stub as one
# target-named tarball.
#
# This first Linux vehicle is deliberately a relocatable archive rather than a
# distro-specific .deb, RPM or confined Snap. It proves the client on WSL2 and
# lets the first-party feed name one immutable artifact without pretending one
# package manager represents Linux. Native installer integration can wrap this
# exact bundle later without changing the pair or feed shape.
#
# Usage:
#   packaging/linux/make-tarball.sh \
#     --bundle <stage>/current \
#     --version 0.8.0 \
#     --target x86_64-unknown-linux-gnu \
#     --out dist
#
# Produces astrolabe-<version>-<target>.tar.gz and its .sha256 sidecar.

set -euo pipefail

BUNDLE="" VERSION="" TARGET="" OUT=""
STUB=""
while [ $# -gt 0 ]; do
  case "$1" in
    --bundle)  BUNDLE="$2"; shift 2 ;;
    --stub)    STUB="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --target)  TARGET="$2"; shift 2 ;;
    --out)     OUT="$2"; shift 2 ;;
    *) echo "make-tarball: unknown argument $1" >&2; exit 1 ;;
  esac
done

[ -n "$BUNDLE" ] && [ -n "$VERSION" ] && [ -n "$TARGET" ] && [ -n "$OUT" ] && [ -n "$STUB" ] || {
  echo "make-tarball: --bundle, --stub, --version, --target and --out are required" >&2
  exit 1
}
[ -x "$STUB" ] || {
  echo "make-tarball: --stub $STUB is not an executable" >&2
  exit 1
}
case "$TARGET" in
  x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu) ;;
  *) echo "make-tarball: unsupported Linux target '$TARGET'" >&2; exit 1 ;;
esac

BUNDLE="$(cd "$BUNDLE" && pwd)"
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"

for required in astrolabe lait; do
  [ -e "$BUNDLE/$required" ] || {
    echo "make-tarball: bundle is missing $required" >&2
    exit 1
  }
done
[ -x "$BUNDLE/astrolabe" ] && [ -x "$BUNDLE/lait" ] || {
  echo "make-tarball: the client and sidecar must both be executable" >&2
  exit 1
}
[ -f "$REPO/THIRD-PARTY-NOTICES.md" ] || {
  echo "make-tarball: THIRD-PARTY-NOTICES.md is missing" >&2
  exit 1
}
# PolyForm's Notices section makes carrying the terms an obligation on whoever
# distributes a copy, not a courtesy — so a missing LICENSE is a refusal, the
# same as missing notices.
[ -f "$REPO/LICENSE" ] || {
  echo "make-tarball: LICENSE is missing" >&2
  exit 1
}

reported="$("$BUNDLE/lait" --version)"
if [ "$reported" != "lait $VERSION" ]; then
  echo "make-tarball: staged lait reports '$reported', package says '$VERSION'" >&2
  exit 1
fi
echo "make-tarball: pair confirmed: $reported"

# Presence is not enough for a native bundle. Refuse any library the build host
# cannot resolve before the archive reaches a clean machine with less context.
for native in astrolabe lait; do
  missing="$(ldd "$BUNDLE/$native" 2>&1 | grep 'not found' || true)"
  [ -z "$missing" ] || {
    echo "make-tarball: unresolved libraries for $native:" >&2
    echo "$missing" >&2
    exit 1
  }
done

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
ROOT_NAME="astrolabe-$VERSION-$TARGET"
STAGED="$WORK/$ROOT_NAME"
mkdir "$STAGED"

# The layout is a stub at the root and the release beneath it:
#
#   astrolabe        the stub. The path a launcher, a .desktop file or a
#                    person's shell alias points at, and the one file an
#                    update never moves.
#   current/         the release: the astrolabe+lait pair, flat, with terms.
#   previous/        kept bootable for rollback; staged/ waits for a launch.
#
# The stub takes the *name* `astrolabe` for the same reason it does on
# Windows: everything outside this install keys on a path, and a path that
# moved per release is the most expensive mistake in this space. The pair
# stays flat and together inside current/, because that is where
# `sidecar::resolve` looks and `update::custody_of` is its inverse.
mkdir "$STAGED/current"

cp -a "$BUNDLE/." "$STAGED/current/"
cp "$REPO/THIRD-PARTY-NOTICES.md" "$STAGED/current/THIRD-PARTY-NOTICES.md"
cp "$REPO/LICENSE" "$STAGED/current/LICENSE"
cp "$STUB" "$STAGED/astrolabe"
# This receipt is deliberately at the installation root, outside the release
# trees an update may swap. Its presence means a canonical installer created
# this shape; an older sparse-overlay install cannot acquire it by updating.
printf '%s\n' "$VERSION" > "$STAGED/canonical-layout-v1"

# DrvFS presents ordinary files as executable unless Windows metadata is
# enabled. Normalize the archive so a WSL-built bundle has the same safe modes
# as native CI: traversable directories, readable payload, two executables.
find "$STAGED" -type d -exec chmod 0755 {} +
find "$STAGED" -type f -exec chmod 0644 {} +
chmod 0755 "$STAGED/astrolabe" "$STAGED/current/astrolabe" "$STAGED/current/lait"

ARCHIVE="$OUT/$ROOT_NAME.tar.gz"
rm -f "$ARCHIVE" "$ARCHIVE.sha256"
tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
  -C "$WORK" -czf "$ARCHIVE" "$ROOT_NAME"
(cd "$OUT" && sha256sum "$ROOT_NAME.tar.gz" > "$ROOT_NAME.tar.gz.sha256")

echo "make-tarball: $ARCHIVE"
