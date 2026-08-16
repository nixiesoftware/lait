#!/usr/bin/env bash
#
# Astrolabe — macOS disk image, signed and (optionally) notarized.
#
# Hand-authored and versioned in the repository, deliberately — the same rule
# as packaging/windows/astrolabe.nsi. What this script signs and what it
# refuses to ship is exactly the surface a clean-machine test exercises, so it
# is reviewed like code rather than generated where nobody reads it.
#
# The macOS install vehicle is a drag-install DMG, not a .pkg: Astrolabe is a
# single-user client with no machine-wide effect (the same reasoning that made
# the Windows installer per-user, no elevation), and a .pkg's one power —
# running root scripts at install — is a power this product must never need.
#
# The Xcode project stays ad-hoc-signed on purpose. Distribution signing
# happens HERE, at package time, with `codesign --options runtime` (the
# hardened runtime notarization requires) — so no personal team identity ever
# lives in the repository, and CI signs with whatever identity the release
# machine holds.
#
# Usage:
#   packaging/macos/make-dmg.sh \
#     --app <path/to/astrolabe.app> \
#     --version <x.y.z> \
#     --identity "Developer ID Application: <name> (<TEAMID>)" \
#     --out <dir> \
#     [--notarize <notarytool-keychain-profile>]
#
# where --app is the Release bundle `flutter build macos --release` produced
# (apps/astrolabe/build/macos/Build/Products/Release/astrolabe.app), with the
# lait sidecar and libastrolabe.dylib already staged inside by rust_build.sh.
#
# Produces <out>/astrolabe-<version>.dmg and its .sha256 sidecar, mirroring
# the Windows job's astrolabe-<version>-setup.exe pair.
#
# The identity must be a "Developer ID Application" certificate. An "Apple
# Development" certificate signs successfully and then fails notarization —
# an error that arrives minutes later naming neither the cert nor this script,
# which is why the type is checked here rather than discovered there.

set -euo pipefail

APP="" VERSION="" IDENTITY="" OUT="" NOTARIZE_PROFILE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --app)      APP="$2"; shift 2 ;;
    --version)  VERSION="$2"; shift 2 ;;
    --identity) IDENTITY="$2"; shift 2 ;;
    --out)      OUT="$2"; shift 2 ;;
    --notarize) NOTARIZE_PROFILE="$2"; shift 2 ;;
    *) echo "make-dmg: unknown argument $1" >&2; exit 1 ;;
  esac
done
[ -n "$APP" ] && [ -n "$VERSION" ] && [ -n "$IDENTITY" ] && [ -n "$OUT" ] || {
  echo "make-dmg: --app, --version, --identity and --out are required" >&2
  exit 1
}
case "$IDENTITY" in
  "Developer ID Application"*) ;;
  *) echo "make-dmg: identity must be a 'Developer ID Application' certificate;" \
          "'$IDENTITY' would sign fine and then fail notarization" >&2; exit 1 ;;
esac

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT"

# --- Stage a copy; never mutate the build tree -------------------------------
#
# Signing rewrites every Mach-O in the bundle. Doing that to the build output
# would make "rebuild, then package" and "package twice" produce different
# bytes, and an incremental Flutter build over a distribution-signed bundle is
# exactly the kind of half-state nobody can reason about later.
STAGED="$WORK/astrolabe.app"
cp -R "$APP" "$STAGED"

# --- The pair, asserted ------------------------------------------------------
#
# The release gate is where claims get checked (CLIENT-12): the bundle must
# carry the sidecar beside the runner — `sidecar::resolve` looks beside the
# executable, and `Contents/MacOS` is the macOS spelling of "beside" — and the
# sidecar must actually run. A bundle that fails either is not a packaging
# input, it is a broken build.
for binary in astrolabe lait libastrolabe.dylib; do
  [ -f "$STAGED/Contents/MacOS/$binary" ] || {
    echo "make-dmg: $binary is missing from Contents/MacOS — not a release bundle" >&2
    exit 1
  }
done
"$STAGED/Contents/MacOS/lait" --version >/dev/null || {
  echo "make-dmg: the staged lait does not run" >&2
  exit 1
}

# What a person who receives these binaries is owed: every crate they are built
# from, and the terms it is offered under. Inside the bundle (not loose in the
# DMG), because a drag-install copies the .app and nothing else — notices that
# stay behind on an unmounted disk image were never really shipped.
cp "$REPO/THIRD-PARTY-NOTICES.md" "$STAGED/Contents/Resources/THIRD-PARTY-NOTICES.md"

# --- Sign inside-out ---------------------------------------------------------
#
# Nested code first, the bundle last: signing the .app seals the state of
# everything under it, so anything signed after the seal invalidates it. No
# `--deep` — Apple deprecated it because it applies one flag set to every
# nesting level sight unseen; the payload here is enumerated for the same
# reason the NSIS file list is: what a package signs is the thing worth
# reading.
#
# `--options runtime` (the hardened runtime) is on every executable because
# notarization requires it on every executable, not just the app. It costs
# nothing here: no JIT in a Flutter release build, and library validation is
# satisfied because every dylib the app loads is signed by this same identity
# a few lines up.
sign() {
  codesign --force --options runtime --timestamp --sign "$IDENTITY" "$@"
}

for framework in "$STAGED"/Contents/Frameworks/*.framework; do
  sign "$framework"
done
sign "$STAGED/Contents/MacOS/libastrolabe.dylib"
sign "$STAGED/Contents/MacOS/lait"
# The app itself carries the entitlements. The sidecar deliberately does not:
# the entitlements file exists to document the sandbox decision (it is off),
# and network client/server are sandbox grants that mean nothing without it.
sign --entitlements "$REPO/apps/astrolabe/macos/Runner/Release.entitlements" "$STAGED"

# Prove the seal before shipping it. `--strict` is the assessment Gatekeeper
# actually runs; a signature that verifies loosely and fails strictly is a
# failure that would otherwise first appear on someone else's machine.
codesign --verify --strict --verbose=1 "$STAGED"

# --- The disk image ----------------------------------------------------------
#
# The canonical drag-install layout: the app and an /Applications symlink, so
# the mounted window is its own instruction. UDZO because it is the format
# every macOS since 10.4 mounts; fancier compression saves megabytes and
# costs compatibility questions.
DMG_SRC="$WORK/dmg"
mkdir "$DMG_SRC"
cp -R "$STAGED" "$DMG_SRC/Astrolabe.app"
ln -s /Applications "$DMG_SRC/Applications"

DMG="$OUT/astrolabe-$VERSION.dmg"
rm -f "$DMG"
hdiutil create -volname "Astrolabe" -srcfolder "$DMG_SRC" -format UDZO -quiet "$DMG"
# The image is signed too: Gatekeeper assesses the container a person
# downloads, not only the app inside it. The identifier is explicit because
# codesign otherwise derives one from the filename truncated at the first
# dot — "astrolabe-0" for astrolabe-0.7.11.dmg — and a version-dependent,
# mangled identifier is noise in every assessment log that names it.
codesign --force --timestamp --identifier com.nixiesoftware.astrolabe.dmg \
  --sign "$IDENTITY" "$DMG"

# --- Notarize, when asked ----------------------------------------------------
#
# Deliberately optional and profile-named, like the feed's signing seed: the
# notary credential lives in a keychain on the invoking machine
# (`xcrun notarytool store-credentials <profile>`), never in this script and
# never in the repository. Notarizing the DMG covers the app inside it; the
# ticket is stapled to the DMG so a first launch works offline. An unnotarized
# run still produces a valid signed image — for CI smoke tests and local
# validation — but Gatekeeper on a customer machine will refuse it, so a
# release without --notarize is not a release.
if [ -n "$NOTARIZE_PROFILE" ]; then
  xcrun notarytool submit "$DMG" --keychain-profile "$NOTARIZE_PROFILE" --wait
  xcrun stapler staple "$DMG"
  # The assessment a customer's Gatekeeper makes, run here first.
  spctl --assess --type open --context context:primary-signature -v "$DMG"
fi

# Same digest sidecar as the Windows job, same format: `<hex>  <name>`.
(cd "$OUT" && shasum -a 256 "astrolabe-$VERSION.dmg" > "astrolabe-$VERSION.dmg.sha256")

echo "make-dmg: $DMG"
