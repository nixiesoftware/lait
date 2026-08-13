# Build one half of the Rust side, with the two guards the bare `cargo build`
# call could not carry.
#
# Invoked as `cmake -P rust_build.cmake` from the custom targets in
# CMakeLists.txt, because both guards need to run at BUILD time. A check written
# into the CMakeLists itself runs at CONFIGURE time, and configure results are
# cached — so "is something holding the exe right now" would be answered once,
# days ago, and never asked again.
#
# Expected on the command line (all -D):
#   WHAT       `core` or `sidecar`
#   WORKSPACE  the cargo workspace root
#   PROFILE    `debug` or `release` (the resolved $<CONFIG>, not a generator expression)

if(NOT WHAT OR NOT WORKSPACE OR NOT PROFILE)
  message(FATAL_ERROR "rust_build.cmake: WHAT, WORKSPACE and PROFILE are all required")
endif()

# === The sidecar is skippable; the core is not ==============================
#
# `lait.exe` is an 85 MB link, and a Dart-only change cannot invalidate it. When
# somebody is iterating on surfaces they can drop it from the loop with
# `set ASTROLABE_SKIP_SIDECAR=1` and keep whatever `lait.exe` is already staged.
#
# Read from the environment at build time rather than a cached CMake option, so
# turning it on and off does not mean reconfiguring the whole project.
if(WHAT STREQUAL "sidecar" AND DEFINED ENV{ASTROLABE_SKIP_SIDECAR}
   AND NOT "$ENV{ASTROLABE_SKIP_SIDECAR}" STREQUAL "0")
  message(STATUS "astrolabe: ASTROLABE_SKIP_SIDECAR is set — keeping the staged lait.exe")
  return()
endif()

# === Refuse legibly when a running client holds what cargo must relink =======
#
# This is the failure the seam produced for real. Astrolabe supervises a
# `lait.exe` sidecar, so a client left running holds BOTH images open. Cargo
# then fails with `failed to remove file …\lait.exe`, MSBuild reports
# `MSB8066 … exited with code 101`, and neither names the app, the lock, or the
# fix. The build cannot proceed either way — but it can say why.
#
# Report, never kill: the running client may be a person's session, and a build
# step that closes somebody's window to save itself twelve seconds is a worse
# surprise than the one it is fixing.
if(WHAT STREQUAL "core")
  set(_images astrolabe.exe)
else()
  set(_images lait.exe)
endif()

foreach(_image IN LISTS _images)
  execute_process(
    COMMAND tasklist /FI "IMAGENAME eq ${_image}" /NH
    OUTPUT_VARIABLE _tasks
    ERROR_QUIET)
  # tasklist answers "INFO: No tasks are running…" on a miss, and a table row
  # naming the image on a hit. Match the image, not the exit code — tasklist
  # returns 0 either way.
  if(_tasks MATCHES "${_image}")
    message(FATAL_ERROR
      "\n"
      "  ${_image} is running, and it holds the file cargo is about to relink.\n"
      "\n"
      "  Astrolabe supervises a lait.exe sidecar, so a running client holds\n"
      "  both images. Close it, or:\n"
      "\n"
      "      taskkill /F /IM astrolabe.exe /IM lait.exe\n"
      "\n"
      "  Iterating on Dart only? `set ASTROLABE_SKIP_SIDECAR=1` keeps the\n"
      "  staged lait.exe and drops an 85 MB link from every build.\n")
  endif()
endforeach()

# === Build ==================================================================
if(WHAT STREQUAL "core")
  set(_packages -p astrolabe)
else()
  set(_packages -p lait)
endif()

set(_profile_flag "")
if(NOT PROFILE STREQUAL "debug")
  set(_profile_flag --release)
endif()

execute_process(
  COMMAND cargo build ${_packages} ${_profile_flag}
  WORKING_DIRECTORY "${WORKSPACE}"
  RESULT_VARIABLE _code)

if(NOT _code EQUAL 0)
  message(FATAL_ERROR "astrolabe: cargo build ${_packages} failed (${_code})")
endif()
