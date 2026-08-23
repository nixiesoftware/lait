#!/usr/bin/env bash
# Assemble first-party World fixtures beside the native debug binaries before
# any process test that explicitly installs them into its throwaway identity.
# Production never discovers this directory; the test harness opts in so a
# developer's previously staged target tree cannot make a clean checkout pass.
set -euo pipefail

OUTPUT="${1:-target/debug}"
HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
[ -n "$HOST_TARGET" ] || {
  echo "rustc did not report its host target" >&2
  exit 1
}

ARTIFACT_ROOT="${ARTIFACT_ROOT:-target/debug}" PROFILE="${PROFILE:-debug}" \
  bash .github/scripts/stage-worlds.sh "$HOST_TARGET" "$OUTPUT"
