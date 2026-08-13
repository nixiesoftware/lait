# Project-level configuration.
cmake_minimum_required(VERSION 3.14)
project(astrolabe LANGUAGES CXX)

# The name of the executable created for the application. Change this to change
# the on-disk name of your application.
set(BINARY_NAME "astrolabe")

# Explicitly opt in to modern CMake behaviors to avoid warnings with recent
# versions of CMake.
cmake_policy(VERSION 3.14...3.25)

# Define build configuration option.
get_property(IS_MULTICONFIG GLOBAL PROPERTY GENERATOR_IS_MULTI_CONFIG)
if(IS_MULTICONFIG)
  set(CMAKE_CONFIGURATION_TYPES "Debug;Profile;Release"
    CACHE STRING "" FORCE)
else()
  if(NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
    set(CMAKE_BUILD_TYPE "Debug" CACHE
      STRING "Flutter build mode" FORCE)
    set_property(CACHE CMAKE_BUILD_TYPE PROPERTY STRINGS
      "Debug" "Profile" "Release")
  endif()
endif()
# Define settings for the Profile build mode.
set(CMAKE_EXE_LINKER_FLAGS_PROFILE "${CMAKE_EXE_LINKER_FLAGS_RELEASE}")
set(CMAKE_SHARED_LINKER_FLAGS_PROFILE "${CMAKE_SHARED_LINKER_FLAGS_RELEASE}")
set(CMAKE_C_FLAGS_PROFILE "${CMAKE_C_FLAGS_RELEASE}")
set(CMAKE_CXX_FLAGS_PROFILE "${CMAKE_CXX_FLAGS_RELEASE}")

# Use Unicode for all projects.
add_definitions(-DUNICODE -D_UNICODE)

# Compilation settings that should be applied to most targets.
#
# Be cautious about adding new options here, as plugins use this function by
# default. In most cases, you should add new options to specific targets instead
# of modifying this function.
function(APPLY_STANDARD_SETTINGS TARGET)
  target_compile_features(${TARGET} PUBLIC cxx_std_17)
  target_compile_options(${TARGET} PRIVATE /W4 /WX /wd"4100")
  target_compile_options(${TARGET} PRIVATE /EHsc)
  target_compile_definitions(${TARGET} PRIVATE "_HAS_EXCEPTIONS=0")
  target_compile_definitions(${TARGET} PRIVATE "$<$<CONFIG:Debug>:_DEBUG>")
endfunction()

# Flutter library and tool build rules.
set(FLUTTER_MANAGED_DIR "${CMAKE_CURRENT_SOURCE_DIR}/flutter")
add_subdirectory(${FLUTTER_MANAGED_DIR})

# Application build; see runner/CMakeLists.txt.
add_subdirectory("runner")


# Generated plugin build rules, which manage building the plugins and adding
# them to the application.
include(flutter/generated_plugins.cmake)


# === Installation ===
# Support files are copied into place next to the executable, so that it can
# run in place. This is done instead of making a separate bundle (as on Linux)
# so that building and running from within Visual Studio will work.
set(BUILD_BUNDLE_DIR "$<TARGET_FILE_DIR:${BINARY_NAME}>")
# Make the "install" step default, as it's required to run.
set(CMAKE_VS_INCLUDE_INSTALL_TO_DEFAULT_BUILD 1)
if(CMAKE_INSTALL_PREFIX_INITIALIZED_TO_DEFAULT)
  set(CMAKE_INSTALL_PREFIX "${BUILD_BUNDLE_DIR}" CACHE PATH "..." FORCE)
endif()

set(INSTALL_BUNDLE_DATA_DIR "${CMAKE_INSTALL_PREFIX}/data")
set(INSTALL_BUNDLE_LIB_DIR "${CMAKE_INSTALL_PREFIX}")

# === The C runtime ==========================================================
#
# `astrolabe.exe` imports MSVCP140.dll, VCRUNTIME140.dll and VCRUNTIME140_1.dll.
# A development machine has them because Visual Studio installed them, which is
# exactly why their absence is invisible here and fatal on a clean one: the app
# fails in the loader, before a frame, with a dialog naming a DLL rather than
# this program.
#
# Deployed app-locally rather than by chaining the redistributable installer,
# because a per-user install that has no machine-wide effect should not need
# elevation — and the redist does. Microsoft permits local deployment of the
# runtime for exactly this case.
#
# SKIP first, then install them ourselves: left to itself the module installs to
# `bin/`, and the rule here is that everything the client loads sits beside the
# executable.
set(CMAKE_INSTALL_SYSTEM_RUNTIME_LIBS_SKIP TRUE)
include(InstallRequiredSystemLibraries)
install(FILES ${CMAKE_INSTALL_SYSTEM_RUNTIME_LIBS}
  DESTINATION "${INSTALL_BUNDLE_LIB_DIR}"
  COMPONENT Runtime)

install(TARGETS ${BINARY_NAME} RUNTIME DESTINATION "${CMAKE_INSTALL_PREFIX}"
  COMPONENT Runtime)

install(FILES "${FLUTTER_ICU_DATA_FILE}" DESTINATION "${INSTALL_BUNDLE_DATA_DIR}"
  COMPONENT Runtime)

install(FILES "${FLUTTER_LIBRARY}" DESTINATION "${INSTALL_BUNDLE_LIB_DIR}"
  COMPONENT Runtime)

if(PLUGIN_BUNDLED_LIBRARIES)
  install(FILES "${PLUGIN_BUNDLED_LIBRARIES}"
    DESTINATION "${INSTALL_BUNDLE_LIB_DIR}"
    COMPONENT Runtime)
endif()

# Copy the native assets provided by the build.dart from all packages.
set(NATIVE_ASSETS_DIR "${PROJECT_BUILD_DIR}native_assets/windows/")
install(DIRECTORY "${NATIVE_ASSETS_DIR}"
   DESTINATION "${INSTALL_BUNDLE_LIB_DIR}"
   COMPONENT Runtime)

# Fully re-copy the assets directory on each build to avoid having stale files
# from a previous install.
set(FLUTTER_ASSET_DIR_NAME "flutter_assets")
install(CODE "
  file(REMOVE_RECURSE \"${INSTALL_BUNDLE_DATA_DIR}/${FLUTTER_ASSET_DIR_NAME}\")
  " COMPONENT Runtime)
install(DIRECTORY "${PROJECT_BUILD_DIR}/${FLUTTER_ASSET_DIR_NAME}"
  DESTINATION "${INSTALL_BUNDLE_DATA_DIR}" COMPONENT Runtime)

# Install the AOT library on non-Debug builds only.
install(FILES "${AOT_LIBRARY}" DESTINATION "${INSTALL_BUNDLE_DATA_DIR}"
  CONFIGURATIONS Profile;Release
  COMPONENT Runtime)

# === The Rust core =========================================================
#
# Astrolabe is a Dart interface over a Rust core, and the core is not this
# app's private engine: `tools/astrolabe` is a workspace member that also
# carries the supervisor, the control-protocol client and the model, and the
# workspace's own tests build it directly. So it is built where it lives and
# the artifact is copied here, rather than being vendored into the app.
#
# Debug Flutter builds link the debug core and Release the release one, because
# a release interface over a debug core is a configuration nobody runs on
# purpose and everybody eventually ships by accident.
set(RUST_WORKSPACE "${CMAKE_CURRENT_SOURCE_DIR}/../../..")

# Generator expressions rather than `CMAKE_BUILD_TYPE`.
#
# Flutter drives Windows through Visual Studio, which is a *multi-config*
# generator: `CMAKE_BUILD_TYPE` is empty at configure time and the configuration
# is only known when MSBuild runs. Reading it produced a build that always took
# the else branch — every Flutter Debug build compiled and installed the
# *release* core, which is a configuration nobody runs on purpose and everybody
# eventually ships by accident.
set(RUST_PROFILE_DIR "$<IF:$<CONFIG:Debug>,debug,release>")
set(RUST_TARGET_DIR "${RUST_WORKSPACE}/target/${RUST_PROFILE_DIR}")

# The core and the sidecar are built by SEPARATE targets, because they are
# invalidated by different things and only one of them is on the inner loop.
#
# `astrolabe.dll` is what a Dart change can require rebuilding; `lait.exe` is an
# 85 MB link that no Dart change can invalidate. Building them under one cargo
# invocation put that link on every single build of the interface, which is the
# cost you pay most often and notice least.
#
# Both go through `rust_build.cmake` rather than calling cargo directly, so the
# running-client check happens at build time. See that file for why.
add_custom_target(astrolabe_core ALL
  COMMAND "${CMAKE_COMMAND}"
    -DWHAT=core
    -DWORKSPACE=${RUST_WORKSPACE}
    -DPROFILE=${RUST_PROFILE_DIR}
    -P "${CMAKE_CURRENT_SOURCE_DIR}/rust_build.cmake"
  COMMENT "Building the Rust core for $<CONFIG>"
  VERBATIM)

add_custom_target(astrolabe_sidecar ALL
  COMMAND "${CMAKE_COMMAND}"
    -DWHAT=sidecar
    -DWORKSPACE=${RUST_WORKSPACE}
    -DPROFILE=${RUST_PROFILE_DIR}
    -P "${CMAKE_CURRENT_SOURCE_DIR}/rust_build.cmake"
  COMMENT "Building the lait.exe sidecar for $<CONFIG>"
  VERBATIM)

add_dependencies(${BINARY_NAME} astrolabe_core astrolabe_sidecar)

install(FILES "${RUST_TARGET_DIR}/astrolabe.dll"
  DESTINATION "${INSTALL_BUNDLE_LIB_DIR}"
  COMPONENT Runtime)
# OPTIONAL, because `ASTROLABE_SKIP_SIDECAR` deliberately leaves whatever
# `lait.exe` is already staged in place. Without it, the one flag that exists to
# make the loop cheaper would fail the install step it was trying to skip.
install(PROGRAMS "${RUST_TARGET_DIR}/lait.exe"
  DESTINATION "${INSTALL_BUNDLE_LIB_DIR}"
  COMPONENT Runtime
  OPTIONAL)
