#!/usr/bin/env bash
# Assemble the carried first-party World releases beside the native debug
# binaries before any test that spawns `lait` or Astrolabe. Cargo builds
# executable targets, but it does not create an application bundle; without
# this explicit step a clean checkout exercises a product-blind host with no
# selected Worlds while a developer's previously staged target tree passes.
set -euo pipefail

OUTPUT="${1:-target/debug}"
HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
[ -n "$HOST_TARGET" ] || {
  echo "rustc did not report its host target" >&2
  exit 1
}

ARTIFACT_ROOT="${ARTIFACT_ROOT:-target/debug}" PROFILE="${PROFILE:-debug}" \
  bash .github/scripts/stage-worlds.sh "$HOST_TARGET" "$OUTPUT"
