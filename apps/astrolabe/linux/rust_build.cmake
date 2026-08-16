# Build one half of the Rust side — the Linux counterpart of
# `windows/rust_build.cmake` and `macos/rust_build.sh`.
#
# Invoked as `cmake -P rust_build.cmake` from the custom targets in
# CMakeLists.txt, so the environment is read at BUILD time. A check written into
# the CMakeLists itself runs at CONFIGURE time and is cached, which would answer
# "is the skip flag set right now" once, days ago, and never ask again.
#
# Expected on the command line (all -D):
#   WHAT       `core` or `sidecar`
#   WORKSPACE  the cargo workspace root
#   PROFILE    `debug` or `release` (the resolved $<CONFIG>, not a generator
#              expression)
#
# What this file does NOT carry, and why:
#
#   * There is no running-image guard. Windows refuses to relink an executable
#     something holds open, which is why that file spends fifty lines saying so
#     legibly. Linux unlinks the directory entry and leaves a running process on
#     its now-anonymous inode, so `cargo build` succeeds while the old client
#     keeps running — the failure the Windows guard exists to explain cannot
#     arise here. The staging copy is handled the same way: Flutter's generated
#     install rules wipe the bundle directory before writing it, and removing a
#     running binary's entry is permitted.

if(NOT WHAT OR NOT WORKSPACE OR NOT PROFILE)
  message(FATAL_ERROR "rust_build.cmake: WHAT, WORKSPACE and PROFILE are all required")
endif()

# === The sidecar is skippable; the core is not ==============================
#
# `lait` is a large link, and a Dart-only change cannot invalidate it. Somebody
# iterating on surfaces drops it from the loop with `ASTROLABE_SKIP_SIDECAR=1`
# and keeps whatever `lait` is already staged.
#
# Read from the environment at build time rather than a cached CMake option, so
# turning it on and off does not mean reconfiguring the whole project.
if(WHAT STREQUAL "sidecar" AND DEFINED ENV{ASTROLABE_SKIP_SIDECAR}
   AND NOT "$ENV{ASTROLABE_SKIP_SIDECAR}" STREQUAL "0")
  message(STATUS "astrolabe: ASTROLABE_SKIP_SIDECAR is set — keeping the staged lait")
  return()
endif()

# === cargo has to be findable ===============================================
#
# The same failure the macOS build phase documents: a build driven by a tool
# that stripped PATH fails with `cargo: command not found` on a machine where
# cargo is plainly on the shell's PATH. That reads like a broken toolchain and
# is a broken environment.
find_program(CARGO_EXECUTABLE cargo)
if(NOT CARGO_EXECUTABLE)
  set(_fallback "$ENV{HOME}/.cargo/bin/cargo")
  if(EXISTS "${_fallback}")
    set(CARGO_EXECUTABLE "${_fallback}")
  else()
    message(FATAL_ERROR
      "astrolabe: cargo is not on PATH and $ENV{HOME}/.cargo/bin does not carry it")
  endif()
endif()

# === Build ==================================================================
#
# The core and the sidecar are separate invocations so the expensive one can be
# skipped. `--bin lait` on the sidecar, because the `lait` package also builds a
# library and this target wants the executable.
if(WHAT STREQUAL "core")
  set(_packages -p astrolabe)
else()
  set(_packages -p lait --bin lait)
endif()

set(_profile_flag "")
if(NOT PROFILE STREQUAL "debug")
  set(_profile_flag --release)
endif()

execute_process(
  COMMAND "${CARGO_EXECUTABLE}" build ${_packages} ${_profile_flag}
  WORKING_DIRECTORY "${WORKSPACE}"
  RESULT_VARIABLE _code)

if(NOT _code EQUAL 0)
  message(FATAL_ERROR "astrolabe: cargo build ${_packages} failed (${_code})")
endif()
