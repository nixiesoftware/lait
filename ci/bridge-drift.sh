#!/usr/bin/env bash
# The generated bridge is checked in, and this is what stops a stale one from
# passing review.
#
# Revision 7 of the Astrolabe Plan reintroduced a boundary between the Rust core
# and the Dart interface, and named five rules that keep it safe. This file is
# the fourth: *the generated bridge is checked in, and drift fails the build*.
# Without it, a change to `tools/astrolabe/src/api` that nobody regenerated is a
# binding that compiles, runs, and disagrees with the model — which is the exact
# defect the pre-revision-7 design had no way to have.
#
# Same shape as `ci/third-party-notices.sh` and `ci/coverage-manifest.sh`: it
# regenerates into the working tree, asks git whether anything moved, and fails
# with the command to fix it. Run with `--update` to regenerate and keep it.
set -euo pipefail

cd "$(dirname "$0")/.."

APP=apps/astrolabe
GENERATED=(
  "$APP/lib/src/bridge"
  tools/astrolabe/src/frb_generated.rs
)

usage() {
  echo "usage: $0 [--check|--update]" >&2
  exit 2
}

mode=${1:---check}
case "$mode" in
  --check | --update) ;;
  *) usage ;;
esac

if ! command -v flutter_rust_bridge_codegen >/dev/null 2>&1; then
  echo "bridge-drift: flutter_rust_bridge_codegen is not installed." >&2
  echo "bridge-drift: cargo install flutter_rust_bridge_codegen --version 2.12.0 --locked" >&2
  exit 1
fi

# The version is pinned in three places and they must agree: the Rust
# dependency, the Dart dependency, and the generator that wrote both halves.
# A generator a minor version ahead emits code the runtime does not implement,
# and the failure lands at run time in a program with no console.
# `sed` and `grep -oE` rather than `grep -oP`: PCRE mode is a GNU extension
# that BSD grep does not carry, so the -P form exited 2 on macOS before it
# checked anything. This script's whole job is to be run by hand until a
# workflow calls it (CLIENT-61), and the hand it is run by is usually on a Mac.
pinned=$(sed -n 's/^flutter_rust_bridge = "=\([0-9.]*\)".*/\1/p' tools/astrolabe/Cargo.toml)
actual=$(flutter_rust_bridge_codegen --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
if [ "$pinned" != "$actual" ]; then
  echo "bridge-drift: the generator is $actual and the pin is $pinned." >&2
  echo "bridge-drift: cargo install flutter_rust_bridge_codegen --version $pinned --locked" >&2
  exit 1
fi

(cd "$APP" && flutter_rust_bridge_codegen generate >/dev/null)

if [ "$mode" = "--update" ]; then
  echo "bridge-drift: regenerated."
  exit 0
fi

if ! git diff --quiet -- "${GENERATED[@]}"; then
  echo "bridge-drift: the checked-in bridge does not match \`crate::api\`." >&2
  echo "bridge-drift: run 'bash ci/bridge-drift.sh --update' and commit the result." >&2
  git --no-pager diff --stat -- "${GENERATED[@]}" >&2
  exit 1
fi

echo "bridge-drift: the bridge matches its source."
