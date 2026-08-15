# Build one Rust half for the Linux Flutter bundle.
#
# Expected on the command line (all -D):
#   WHAT       `core` or `sidecar`
#   WORKSPACE  the cargo workspace root
#   PROFILE    `debug` or `release`

if(NOT WHAT OR NOT WORKSPACE OR NOT PROFILE)
  message(FATAL_ERROR "rust_build.cmake: WHAT, WORKSPACE and PROFILE are all required")
endif()

if(WHAT STREQUAL "sidecar" AND DEFINED ENV{ASTROLABE_SKIP_SIDECAR}
   AND NOT "$ENV{ASTROLABE_SKIP_SIDECAR}" STREQUAL "0")
  message(STATUS "astrolabe: ASTROLABE_SKIP_SIDECAR is set — keeping the staged lait")
  return()
endif()

find_program(CARGO_EXECUTABLE cargo HINTS "$ENV{HOME}/.cargo/bin")
if(NOT CARGO_EXECUTABLE)
  message(FATAL_ERROR "astrolabe: cargo is not on PATH or in $ENV{HOME}/.cargo/bin")
endif()

if(WHAT STREQUAL "core")
  set(_packages -p astrolabe)
else()
  set(_packages -p lait --bin lait)
endif()

if(PROFILE STREQUAL "debug")
  set(_cargo_profile dev)
else()
  set(_cargo_profile release)
endif()

execute_process(
  COMMAND "${CARGO_EXECUTABLE}" build ${_packages}
    --profile "${_cargo_profile}"
    --manifest-path "${WORKSPACE}/Cargo.toml"
  WORKING_DIRECTORY "${WORKSPACE}"
  RESULT_VARIABLE _code)

if(NOT _code EQUAL 0)
  message(FATAL_ERROR "astrolabe: cargo build ${_packages} failed (${_code})")
endif()
