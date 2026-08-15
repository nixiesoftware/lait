#!/bin/bash
#
# Build the Rust halves and stage them inside the .app — the macOS counterpart
# of `windows/rust_build.cmake`.
#
# Run as an Xcode build phase on the Runner target, so it reads `CONFIGURATION`,
# `SRCROOT` and `BUILT_PRODUCTS_DIR` from the environment rather than taking
# arguments. Everything it decides, that file decides too; the differences are
# the ones the platform forces:
#
#   * There is no running-image guard. Windows refuses to relink an executable
#     something holds open, which is why that file spends fifty lines saying so
#     legibly. macOS lets the link succeed and only refuses the *copy* into a
#     running bundle (ETXTBSY), so the staging step unlinks first and the
#     failure never arises.
#   * The destination is `Contents/MacOS`, not a lib directory. Both consumers
#     look beside the executable — `Client._core()` in Dart for the dylib, and
#     `sidecar::beside` in Rust for `lait` — so that is where they go.
set -euo pipefail

# Xcode's build phases run under a stripped PATH that does not include a rustup
# shim directory. Without this the phase fails with `cargo: command not found`
# on a machine where cargo is plainly on the shell's PATH, which reads like a
# broken toolchain and is a broken environment.
if ! command -v cargo >/dev/null 2>&1; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is not on PATH and \$HOME/.cargo/bin does not carry it" >&2
  exit 1
fi

WORKSPACE="$(cd "${SRCROOT}/../../.." && pwd)"

# Debug Flutter builds link the debug core and Release the release one. A
# release interface over a debug core is a configuration nobody runs on purpose
# and everybody eventually ships by accident — the same reasoning, and the same
# sentence, as the Windows side.
#
# Named through `--profile` rather than a bare `--release`, because the empty
# half of that choice is an empty array — and macOS ships bash 3.2, where
# expanding one under `set -u` is an unbound-variable error rather than nothing.
if [ "${CONFIGURATION}" = "Debug" ]; then
  CARGO_PROFILE="dev"
  PROFILE="debug"
else
  CARGO_PROFILE="release"
  PROFILE="release"
fi

TARGET_DIR="${WORKSPACE}/target/${PROFILE}"
STAGE="${BUILT_PRODUCTS_DIR}/${EXECUTABLE_FOLDER_PATH}"

# Xcode exports a compiler environment aimed at the Runner's own build, and
# `cc`-driven build scripts under cargo pick it up. Two rules, and the second
# one was learned by breaking it:
#
#   * The search paths go. They point at the Runner's dependencies, not this
#     workspace's, and a build script that finds the wrong header there fails
#     naming neither Xcode nor cargo.
#   * `SDKROOT` stays — set explicitly, so it is the macOS SDK and not whatever
#     Xcode happened to be pointed at. Unsetting it leaves `cc` with no sysroot
#     at all, and `dart-sys` fails to find `assert.h`, which reads like a
#     corrupt toolchain and is a missing one line.
unset CPATH LIBRARY_PATH LD_LIBRARY_PATH
SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
export SDKROOT

# `astrolabe`'s cdylib is what a Dart change can require rebuilding; `lait` is a
# large link that no Dart change can invalidate. Separate invocations, so the
# expensive one can be skipped.
echo "astrolabe: building the Rust core for ${CONFIGURATION}"
cargo build -p astrolabe --profile "${CARGO_PROFILE}" --manifest-path "${WORKSPACE}/Cargo.toml"

# `ASTROLABE_SKIP_SIDECAR=1` keeps whatever `lait` is already staged, for
# somebody iterating on surfaces. Read from the environment at build time so
# turning it on and off does not mean reconfiguring anything.
if [ "${ASTROLABE_SKIP_SIDECAR:-0}" != "0" ]; then
  echo "astrolabe: ASTROLABE_SKIP_SIDECAR is set — keeping the staged lait"
else
  echo "astrolabe: building the lait sidecar for ${CONFIGURATION}"
  cargo build -p lait --bin lait --profile "${CARGO_PROFILE}" --manifest-path "${WORKSPACE}/Cargo.toml"
fi

mkdir -p "${STAGE}"

# Unlink before copying. A `cp` onto the image of a process that is still
# running answers ETXTBSY; removing the directory entry first leaves the running
# process on its now-anonymous inode and puts the new build in its place.
stage() {
  local from="$1" to="$2"
  if [ ! -f "${from}" ]; then
    echo "error: ${from} was not produced by the build" >&2
    exit 1
  fi
  rm -f "${to}"
  cp "${from}" "${to}"
  # Ad-hoc, because these are copied in after the Runner target was signed and
  # an unsigned Mach-O inside a signed bundle is refused at launch. A real
  # distribution build re-signs the whole bundle afterwards and overwrites this.
  codesign --force --sign - --timestamp=none "${to}" >/dev/null 2>&1 || true
}

stage "${TARGET_DIR}/libastrolabe.dylib" "${STAGE}/libastrolabe.dylib"
if [ "${ASTROLABE_SKIP_SIDECAR:-0}" = "0" ]; then
  stage "${TARGET_DIR}/lait" "${STAGE}/lait"
fi

echo "astrolabe: staged the core and the sidecar in ${STAGE}"
