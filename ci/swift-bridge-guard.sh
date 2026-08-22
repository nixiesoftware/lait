#!/usr/bin/env bash
# The iOS one-file bridge rule, enforced.
#
# `apps/astrolabe-ios/Sources/CoreBridge.swift` declares itself the only file
# that may call the generated UniFFI bridge — the Swift analogue of
# `client.dart`'s rule on desktop. Dart's rule is checkable by import; UniFFI
# emits free functions into the app's own module, so any Swift file *can* call
# `clientView()` and the compiler will never object. That makes the rule a
# machine's job: derive every generated entry point from the checked-in
# `Generated/` Swift, then flag a call from any other file under `Sources/`.
#
# Generated *types* flow outward freely, exactly as the rule's header says —
# only calls are scanned. Pure grep: no Apple toolchain, runs anywhere.
set -euo pipefail

APP=apps/astrolabe-ios
GENERATED="$APP/Generated/astrolabe_ios.swift"
BRIDGE_FILE="CoreBridge.swift"

if [ ! -f "$GENERATED" ]; then
  echo "error: $GENERATED is missing — the checked-in boundary should exist" >&2
  exit 1
fi

names=$(grep -oE '^public func [A-Za-z_][A-Za-z0-9_]*' "$GENERATED" | awk '{print $3}' | sort -u)

fail=0
for name in $names; do
  hits=$(grep -rnE "\b${name}[[:space:]]*\(" "$APP/Sources" | grep -v "/${BRIDGE_FILE}:" || true)
  if [ -n "$hits" ]; then
    echo "bridge call outside ${BRIDGE_FILE}: ${name}" >&2
    echo "$hits" >&2
    fail=1
  fi
done

if [ "$fail" -ne 0 ]; then
  echo >&2
  echo "The generated bridge may only be called from ${APP}/Sources/${BRIDGE_FILE}." >&2
  echo "Route the call through the Core facade there instead." >&2
  exit 1
fi

echo "swift-bridge-guard: every generated call is inside ${BRIDGE_FILE}"
