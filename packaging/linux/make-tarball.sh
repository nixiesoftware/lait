#!/usr/bin/env bash
#
# Astrolabe — relocatable Linux desktop bundle.
#
# Flutter's Linux release is a directory, not one executable: the runner,
# engine and plugin libraries, AOT image, ICU data and assets must travel as a
# unit. This script refuses an incomplete unit, adds the Rust core + lait pair
# and notices that the build staged, then seals the whole directory as one
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
#     --bundle apps/astrolabe/build/linux/x64/release/bundle \
#     --version 0.8.0 \
#     --target x86_64-unknown-linux-gnu \
#     --out dist
#
# Produces astrolabe-<version>-<target>.tar.gz and its .sha256 sidecar.

set -euo pipefail

BUNDLE="" VERSION="" TARGET="" OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --bundle)  BUNDLE="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --target)  TARGET="$2"; shift 2 ;;
    --out)     OUT="$2"; shift 2 ;;
    *) echo "make-tarball: unknown argument $1" >&2; exit 1 ;;
  esac
done

[ -n "$BUNDLE" ] && [ -n "$VERSION" ] && [ -n "$TARGET" ] && [ -n "$OUT" ] || {
  echo "make-tarball: --bundle, --version, --target and --out are required" >&2
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

for required in astrolabe lait libastrolabe.so data lib; do
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

reported="$("$BUNDLE/lait" --version)"
if [ "$reported" != "lait $VERSION" ]; then
  echo "make-tarball: staged lait reports '$reported', package says '$VERSION'" >&2
  exit 1
fi
echo "make-tarball: pair confirmed: $reported"

# Presence is not enough for a native bundle. Refuse any library the build host
# cannot resolve before the archive reaches a clean machine with less context.
for native in astrolabe lait libastrolabe.so; do
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

# The directory is Flutter's unit of distribution. Enumerating today's engine
# files would make tomorrow's newly required file the one an old script drops.
cp -a "$BUNDLE/." "$STAGED/"
cp "$REPO/THIRD-PARTY-NOTICES.md" "$STAGED/THIRD-PARTY-NOTICES.md"

# DrvFS presents ordinary files as executable unless Windows metadata is
# enabled. Normalize the archive so a WSL-built bundle has the same safe modes
# as native CI: traversable directories, readable payload, two executables.
find "$STAGED" -type d -exec chmod 0755 {} +
find "$STAGED" -type f -exec chmod 0644 {} +
chmod 0755 "$STAGED/astrolabe" "$STAGED/lait"

ARCHIVE="$OUT/$ROOT_NAME.tar.gz"
rm -f "$ARCHIVE" "$ARCHIVE.sha256"
tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
  -C "$WORK" -czf "$ARCHIVE" "$ROOT_NAME"
(cd "$OUT" && sha256sum "$ROOT_NAME.tar.gz" > "$ROOT_NAME.tar.gz.sha256")

echo "make-tarball: $ARCHIVE"
