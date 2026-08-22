#!/usr/bin/env bash
#
# build-astrolabe.sh — build the Tauri client and emit the artifacts the FEED
# serves, named the way the feed names them.
#
# Distribution is the first-party feed (SUB-13), not a git forge: installed
# machines follow a signed channel pointer on the dist host and never look at
# a release page. This script is the build half of that; `ci/publish-feed.sh
# --version <v> --artifacts-dir <dir>` is the publish half, and the two meet
# at a directory of files with these names:
#
#   astrolabe-<version>.dmg                      what a person installs (macOS)
#   astrolabe-<version>-setup.exe                          "        "   (Windows)
#   astrolabe-tree-<version>-<target>.tar.gz     what a running machine swaps in
#
# The tree is not optional. A release carrying an installer without one can be
# installed and never updated from — `make-tree.sh` says so at length, and this
# script always produces it.
#
# ## The pair ships together, and this asserts it
#
# Astrolabe bundles `lait` inside and resolves it strictly beside its own
# executable. Tauri's `externalBin` puts it there; `stage-sidecar.mjs --bundle`
# builds it from THIS tree first, so both halves come from one source (the pair
# rule, CLIENT-12). "By construction" is a claim, so the claim is checked
# below: the staged sidecar is run and its version compared to --version.
#
# Usage:
#   packaging/build-astrolabe.sh --version 0.9.0 --out target/distrib \
#     [--identity "Developer ID Application: … (TEAMID)"] [--notarize <profile>]
#
# Without --identity on macOS the .dmg is UNSIGNED: fine for a local check,
# never for the feed, and this says so rather than producing something that
# looks publishable.

set -euo pipefail

VERSION="" OUT="" IDENTITY="" NOTARIZE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --version)  VERSION="$2"; shift 2 ;;
    --out)      OUT="$2"; shift 2 ;;
    --identity) IDENTITY="$2"; shift 2 ;;
    --notarize) NOTARIZE="$2"; shift 2 ;;
    *) echo "build-astrolabe: unknown argument $1" >&2; exit 1 ;;
  esac
done
[ -n "$VERSION" ] && [ -n "$OUT" ] || {
  echo "build-astrolabe: --version and --out are required" >&2; exit 1
}

REPO="$(cd "$(dirname "$0")/.." && pwd)"
CLIENT="$REPO/apps/astrolabe-web"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"

TARGET="$(rustc -vV | sed -n 's/^host: //p')"
case "$TARGET" in *-windows-*) EXE=".exe" ;; *) EXE="" ;; esac
# The stub's live tree, spelled once. Must agree with `update::tree::LIVE_DIR`
# and the stub's own constant.
LIVE_DIR="current"
[ -n "$TARGET" ] || { echo "build-astrolabe: rustc did not report a host triple" >&2; exit 1; }

# The bundle carries the release version, not whatever the checked-in config
# last said. Overridden on the command line rather than by rewriting the file,
# so a build never leaves the tree dirty.
echo "build-astrolabe: building $VERSION for $TARGET"
(cd "$CLIENT" && npm run tauri -- build --config "{\"version\":\"$VERSION\"}")

BUNDLE="$CLIENT/src-tauri/target/release/bundle"

case "$TARGET" in
  *-apple-*)
    APP="$BUNDLE/macos/Astrolabe.app"
    [ -d "$APP" ] || { echo "build-astrolabe: no .app at $APP" >&2; exit 1; }
    STAGED_LAIT="$APP/Contents/MacOS/lait"
    ;;
  *-windows-*)
    APP="$BUNDLE/nsis"
    STAGED_LAIT="$CLIENT/src-tauri/target/release/lait.exe"
    ;;
  *)
    APP="$BUNDLE/appimage"
    STAGED_LAIT="$CLIENT/src-tauri/target/release/lait"
    ;;
esac

# The pair rule, checked rather than assumed. A client shipped with a sidecar
# from another tree is the failure this exists to keep out of a release, and it
# is invisible until something asks the daemon for its version.
[ -x "$STAGED_LAIT" ] || {
  echo "build-astrolabe: no staged lait at $STAGED_LAIT — the pair did not ship together" >&2
  exit 1
}
reported="$("$STAGED_LAIT" --version)"
case "$reported" in
  "lait $VERSION"|"lait $VERSION "*) ;;
  *) echo "build-astrolabe: the bundled sidecar reports '$reported', not 'lait $VERSION'." >&2
     echo "  The pair must be built from one tree. Check the workspace version." >&2
     exit 1 ;;
esac
echo "build-astrolabe: pair verified — bundled sidecar reports $reported"

case "$TARGET" in
  *-apple-*)
    if [ -n "$IDENTITY" ]; then
      ARGS=(--app "$APP" --version "$VERSION" --identity "$IDENTITY" --out "$OUT")
      [ -n "$NOTARIZE" ] && ARGS+=(--notarize "$NOTARIZE")
      bash "$REPO/packaging/macos/make-dmg.sh" "${ARGS[@]}"
    else
      # Tauri's own dmg, renamed to the feed's spelling. Unsigned: Gatekeeper
      # refuses it on any machine but this one.
      src="$(ls "$BUNDLE"/dmg/*.dmg 2>/dev/null | head -1)"
      [ -n "$src" ] || { echo "build-astrolabe: no .dmg produced" >&2; exit 1; }
      cp "$src" "$OUT/astrolabe-$VERSION.dmg"
      (cd "$OUT" && shasum -a 256 "astrolabe-$VERSION.dmg" > "astrolabe-$VERSION.dmg.sha256")
      echo "build-astrolabe: WARNING — astrolabe-$VERSION.dmg is UNSIGNED." >&2
      echo "  Gatekeeper refuses an unsigned bundle. Do not publish this to the feed;" >&2
      echo "  pass --identity (and --notarize) for a releasable disk image." >&2
    fi
    bash "$REPO/packaging/make-tree.sh" --stage "$APP" \
      --version "$VERSION" --target "$TARGET" --out "$OUT"
    ;;
  *)
    # Windows and Linux install a stable root: the stub takes the
    # application's name and sits at the root, the release lives beneath it in
    # `current/`, and the pair sits flat and together inside — which is where
    # `sidecar::beside` looks and `update::custody_of` is its inverse. Nothing
    # outside the install may point into a release directory, so the stub's
    # path is the only one any shell artifact ever names.
    STAGE="$OUT/stage"
    rm -rf "$STAGE"
    mkdir -p "$STAGE/$LIVE_DIR"

    echo "build-astrolabe: assembling the stub layout"
    cargo build --release --locked -p astrolabe-stub --bin astrolabe-stub
    cp "$REPO/target/release/astrolabe-stub$EXE" "$STAGE/astrolabe$EXE"

    host="$CLIENT/src-tauri/target/release/astrolabe$EXE"
    [ -f "$host" ] || { echo "build-astrolabe: no client binary at $host" >&2; exit 1; }
    cp "$host" "$STAGE/$LIVE_DIR/astrolabe$EXE"
    cp "$STAGED_LAIT" "$STAGE/$LIVE_DIR/lait$EXE"
    # The terms travel with the tree, not only with the installer: the tree is
    # what a self-update swaps into `current/`, so a file the installer added
    # afterwards would survive the install and not the first upgrade.
    cp "$REPO/LICENSE" "$REPO/THIRD-PARTY-NOTICES.md" "$STAGE/$LIVE_DIR/"

    bash "$REPO/packaging/make-tree.sh" --stage "$STAGE/$LIVE_DIR" \
      --version "$VERSION" --target "$TARGET" --out "$OUT"

    echo "build-astrolabe: the installer for $TARGET is not built here." >&2
    echo "  The stub layout is assembled at $STAGE and the update tree is in" >&2
    echo "  $OUT. Windows takes that stage through packaging/windows/astrolabe.nsi" >&2
    echo "  and Linux through packaging/linux/make-tarball.sh; neither is" >&2
    echo "  exercised on this host, so neither is claimed to work." >&2
    ;;
esac

echo "build-astrolabe: artifacts in $OUT"
ls -1 "$OUT"
