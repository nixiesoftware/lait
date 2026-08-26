#!/usr/bin/env bash
# Assemble the trusted first-party bootstrap releases for one native target.
# The output is the exact tree every client installs through the ordinary
# immutable World loader: worlds/<id>/<version>/...
set -euo pipefail

TARGET="${1:?usage: stage-worlds.sh <target-triple> <output-dir>}"
OUTPUT="${2:?usage: stage-worlds.sh <target-triple> <output-dir>}"
PROFILE="${PROFILE:-release}"
ARTIFACT_ROOT="${ARTIFACT_ROOT:-target/$TARGET/$PROFILE}"
EXE=""
case "$TARGET" in
  *-windows-*) EXE=".exe" ;;
esac

version_of() {
  sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$1" | head -n 1
}

artifact() {
  local runner="$1"
  local path="$ARTIFACT_ROOT/$runner$EXE"
  [ -f "$path" ] || {
    echo "no built $runner$EXE for $TARGET at $path" >&2
    return 1
  }
  printf '%s\n' "$path"
}

stage() {
  local id="$1" version="$2" template="$3" runner="$4"
  local root="$OUTPUT/worlds/$id/$version"
  mkdir -p "$root/bin"
  cp "$(artifact "$runner")" "$root/bin/$runner$EXE"
  sed -e "s/\${VERSION}/$version/g" -e "s/\${EXE}/$EXE/g" \
    "$template" > "$root/world.json"
}

case "$OUTPUT" in
  ""|"/"|".") echo "refusing unsafe World staging output: $OUTPUT" >&2; exit 1 ;;
esac
rm -rf "$OUTPUT/worlds"

ISSUES_VERSION="$(version_of products/issues/Cargo.toml)"
SIGNAGE_VERSION="$(version_of products/signage/Cargo.toml)"
[ -n "$ISSUES_VERSION" ] || { echo "issues package has no version" >&2; exit 1; }
[ -n "$SIGNAGE_VERSION" ] || { echo "signage package has no version" >&2; exit 1; }

stage "com.lait.issues" "$ISSUES_VERSION" \
  products/issues-runner/world.json.template lait-world-issues
cp -R products/issues-app/assets/web/. "$OUTPUT/worlds/com.lait.issues/$ISSUES_VERSION/"
mkdir -p "$OUTPUT/worlds/com.lait.issues/$ISSUES_VERSION/art"
cp products/issues-app/assets/mark.png \
  "$OUTPUT/worlds/com.lait.issues/$ISSUES_VERSION/art/mark.png"
cp products/issues-app/assets/hero.png \
  "$OUTPUT/worlds/com.lait.issues/$ISSUES_VERSION/art/hero.png"

stage "com.lait.signage" "$SIGNAGE_VERSION" \
  products/signage-runner/world.json.template lait-world-signage
# Signage declares a primary web launch target, so the release has to carry the
# bytes that target resolves to. A head serves static files only from the
# selected immutable release; without this copy the Open button reaches a head
# with no index.html to answer with.
cp -R products/signage-app/assets/web/. "$OUTPUT/worlds/com.lait.signage/$SIGNAGE_VERSION/"

echo "staged first-party Worlds for $TARGET at $OUTPUT/worlds"
