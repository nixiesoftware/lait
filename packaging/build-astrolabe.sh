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
# The stub's live tree, spelled once. Must agree with `update::tree::LIVE_DIR`
# and the stub's own constant.
LIVE_DIR="current"
[ -n "$TARGET" ] || { echo "build-astrolabe: rustc did not report a host triple" >&2; exit 1; }

# The feed has one stable artifact name for each supported client platform.
# Refuse any other host before doing an expensive build: emitting a tree under
# an unrecognised triple would create bytes no installed client can discover.
case "$TARGET" in
  aarch64-apple-darwin|x86_64-pc-windows-msvc|x86_64-unknown-linux-gnu) ;;
  *)
    echo "build-astrolabe: unsupported client target '$TARGET'" >&2
    echo "  Feed targets: aarch64-apple-darwin, x86_64-pc-windows-msvc," >&2
    echo "  x86_64-unknown-linux-gnu" >&2
    exit 1
    ;;
esac
case "$TARGET" in *-windows-*) EXE=".exe" ;; *) EXE="" ;; esac

# The installed pair carries its own C runtime on Windows: the stub-managed
# layout ships no msvcp/vcruntime DLLs (the old Flutter bundle staged them),
# and a clean machine must not die in the loader over one. Scoped to this
# vehicle — the bare host archive keeps its own portability posture.
case "$TARGET" in
  *-windows-*) export RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static" ;;
esac

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
    TREE_APP="$APP"
    if [ -n "$IDENTITY" ]; then
      SIGNED_WORK="$(mktemp -d)"
      trap 'rm -rf "$SIGNED_WORK"' EXIT
      SIGNED_APP="$SIGNED_WORK/Astrolabe.app"
      ARGS=(--app "$APP" --version "$VERSION" --identity "$IDENTITY" --out "$OUT"
        --signed-app-out "$SIGNED_APP")
      [ -n "$NOTARIZE" ] && ARGS+=(--notarize "$NOTARIZE")
      bash "$REPO/packaging/macos/make-dmg.sh" "${ARGS[@]}"
      TREE_APP="$SIGNED_APP"
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
    bash "$REPO/packaging/make-tree.sh" --stage "$TREE_APP" \
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
    # `tauri build` did stage the reviewed bootstrap Worlds, but this branch
    # deliberately rebuilds the stable stub layout from the raw host binary.
    # A raw binary carries none of Tauri's resources with it. Preserve that
    # resource tree explicitly in the directory both the installer and the
    # self-update archive consume; otherwise a clean install has no selected
    # World release and the daemon cannot start.
    BUNDLED_WORLDS="$CLIENT/src-tauri/bundled-worlds"
    [ -d "$BUNDLED_WORLDS" ] || {
      echo "build-astrolabe: Tauri staged no first-party Worlds at $BUNDLED_WORLDS" >&2
      exit 1
    }
    cp -R "$BUNDLED_WORLDS" "$STAGE/$LIVE_DIR/worlds"
    # The terms travel with the tree, not only with the installer: the tree is
    # what a self-update swaps into `current/`, so a file the installer added
    # afterwards would survive the install and not the first upgrade.
    cp "$REPO/LICENSE" "$REPO/THIRD-PARTY-NOTICES.md" "$STAGE/$LIVE_DIR/"

    bash "$REPO/packaging/make-tree.sh" --stage "$STAGE/$LIVE_DIR" \
      --version "$VERSION" --target "$TARGET" --out "$OUT"

    case "$TARGET" in
      *-windows-*)
        # WebView2 is the one runtime the pair does not carry; the installer
        # runs Microsoft's bootstrapper on a machine without it. Staged here
        # from Microsoft's permalink, best-effort: without it the installer
        # still builds, and a bare machine is told where to get WebView2
        # instead of being handed it.
        curl -fsSL "https://go.microsoft.com/fwlink/p/?LinkId=2124703" \
          -o "$STAGE/$LIVE_DIR/MicrosoftEdgeWebview2Setup.exe" \
          || echo "build-astrolabe: WARNING — WebView2 bootstrapper not staged; the installer will point instead of provide" >&2

        MAKENSIS="$(command -v makensis || true)"
        # Tauri's CLI caches its own NSIS; use it when none is on PATH.
        if [ -z "$MAKENSIS" ] && [ -n "${LOCALAPPDATA:-}" ]; then
          MAKENSIS="$(find "$LOCALAPPDATA/tauri" -name makensis.exe 2>/dev/null | head -1 || true)"
        fi
        if [ -n "$MAKENSIS" ]; then
          # VIProductVersion needs a numeric x.y.z; a prerelease passes its base.
          NUMERIC="${VERSION%%-*}"
          "$MAKENSIS" -DVERSION="$VERSION" -DVERSION_NUMERIC="$NUMERIC" \
            -DSTAGE="$(cygpath -w "$STAGE/$LIVE_DIR" 2>/dev/null || echo "$STAGE/$LIVE_DIR")" \
            -DSTUB="$(cygpath -w "$REPO/target/release/astrolabe-stub.exe" 2>/dev/null || echo "$REPO/target/release/astrolabe-stub.exe")" \
            -DOUTDIR="$(cygpath -w "$OUT" 2>/dev/null || echo "$OUT")" \
            "$REPO/packaging/windows/astrolabe.nsi"
          (cd "$OUT" && sha256sum "astrolabe-$VERSION-setup.exe" > "astrolabe-$VERSION-setup.exe.sha256")
        else
          echo "build-astrolabe: no makensis found; the stage at $STAGE is ready for" >&2
          echo "  packaging/windows/astrolabe.nsi but no installer was produced." >&2
          exit 1
        fi
        ;;
      *)
        bash "$REPO/packaging/linux/make-tarball.sh" --bundle "$STAGE/$LIVE_DIR" \
          --stub "$REPO/target/release/astrolabe-stub" \
          --version "$VERSION" --target "$TARGET" --out "$OUT"
        ;;
    esac
    ;;
esac

echo "build-astrolabe: artifacts in $OUT"
ls -1 "$OUT"
