#!/bin/bash
#
# Build the Rust core for the platform being built and stage what the app
# needs: the static archive, the FFI header and modulemap, and the generated
# Swift boundary — the iOS counterpart of `macos/rust_build.sh`, minus the
# sidecar, because there is no sidecar: the node is in-process.
#
# Run as an Xcode pre-build phase on the Astrolabe target, so it reads
# `CONFIGURATION`, `PLATFORM_NAME` and `PROJECT_DIR` from the environment.
# Outside Xcode, defaults let `./build-core.sh` stage a Debug device build.
set -euo pipefail

# Xcode's build phases run under a stripped PATH that does not include a rustup
# shim directory — the same line, for the same reason, as the macOS script.
if ! command -v cargo >/dev/null 2>&1; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is not on PATH and \$HOME/.cargo/bin does not carry it" >&2
  exit 1
fi

PROJECT_DIR="${PROJECT_DIR:-$(cd "$(dirname "$0")" && pwd)}"
WORKSPACE="$(cd "${PROJECT_DIR}/../.." && pwd)"

# Debug interface over a debug core, Release over release — the pair rule.
CONFIGURATION="${CONFIGURATION:-Debug}"
if [ "${CONFIGURATION}" = "Debug" ]; then
  CARGO_PROFILE="dev"
  PROFILE="debug"
else
  CARGO_PROFILE="release"
  PROFILE="release"
fi

# Device and simulator are different targets producing incompatible slices;
# they are never conflated, which is why the staging path carries both the
# platform and the configuration.
PLATFORM_NAME="${PLATFORM_NAME:-iphoneos}"
if [ "${PLATFORM_NAME}" = "iphonesimulator" ]; then
  TARGET="aarch64-apple-ios-sim"
else
  TARGET="aarch64-apple-ios"
fi

# Xcode exports a compiler environment aimed at the app's own build. The
# search paths go; SDKROOT is re-set explicitly to the *target's* SDK, which
# is what the workspace's C build scripts (ring, blake3) compile for.
unset CPATH LIBRARY_PATH LD_LIBRARY_PATH
SDKROOT="$(xcrun --sdk "${PLATFORM_NAME}" --show-sdk-path)"
export SDKROOT

# Line tables only: full debuginfo made the debug static archive 2 GB and
# once filled the disk. Backtraces stay symbolicated; stepping through the
# engine happens on desktop, not on a phone.
export CARGO_PROFILE_DEV_DEBUG=line-tables-only

echo "astrolabe-ios: building the Rust core for ${CONFIGURATION} (${TARGET})"
cargo build -p astrolabe-ios --profile "${CARGO_PROFILE}" --target "${TARGET}" \
  --manifest-path "${WORKSPACE}/Cargo.toml"

LIB="${WORKSPACE}/target/${TARGET}/${PROFILE}/libastrolabe_ios.a"
if [ ! -f "${LIB}" ]; then
  echo "error: ${LIB} was not produced by the build" >&2
  exit 1
fi

STAGE="${PROJECT_DIR}/Core/${PLATFORM_NAME}-${CONFIGURATION}"
INCLUDE="${PROJECT_DIR}/Core/include"
mkdir -p "${STAGE}" "${INCLUDE}"
cp -f "${LIB}" "${STAGE}/libastrolabe_ios.a"

# The generated Swift is checked in and reviewed as a build product of the
# pinned generator; regenerating on every build is what keeps it honest — CI's
# drift check is `git diff --exit-code` over Generated/ after this runs. The
# bindgen binary comes from the same pinned uniffi the library uses, so the
# generated Swift can never disagree with the scaffolding it must match.
echo "astrolabe-ios: generating the Swift boundary"
BINDGEN=(cargo run -q -p astrolabe-ios --features bindgen-cli \
  --bin uniffi-bindgen-swift --manifest-path "${WORKSPACE}/Cargo.toml" --)
"${BINDGEN[@]}" --swift-sources "${LIB}" "${PROJECT_DIR}/Generated"
# The module name must be the one the generated Swift imports — the FFI
# module, not the crate.
"${BINDGEN[@]}" --headers --modulemap --module-name astrolabe_iosFFI \
  --modulemap-filename module.modulemap "${LIB}" "${INCLUDE}"

echo "astrolabe-ios: staged ${STAGE} and regenerated the boundary"
