# SuperDuper DSP — VST3 / AUv2 wrappers around the Rust CLAP plugins.
#
# This CMake project does NOT build the plugins themselves — those are
# Rust cdylibs produced by Cargo and packaged into .clap bundles by the
# scripts/build_*_bundle.sh helpers. What this project DOES is generate
# matching VST3 and (on macOS) AUv2 bundles that dynamically load each
# .clap at runtime, courtesy of free-audio/clap-wrapper.
#
# Build flow:
#   1. Build all Rust CLAPs first:           ./scripts/build_release.sh
#   2. Configure + build the wrappers:       ./scripts/build_wrappers.sh
#
# CMake variables (override on the cmake command line if needed):
#   - CLAP_WRAPPER_DOWNLOAD_DEPENDENCIES  ON by default — pulls VST3 SDK,
#       AudioUnitSDK, and CLAP headers via CPM at configure time.
#   - CLAP_WRAPPER_BUILD_AUV2             ON on Apple platforms (off elsewhere).
#   - SDSP_WRAPPERS_INSTALL_LOCAL         OFF by default. ON copies built
#       bundles into ~/Library/Audio/Plug-Ins/{VST3,Components} on macOS
#       for fast turnaround on the host machine.

cmake_minimum_required(VERSION 3.21)

# clap-wrapper wants C++17 and Objective-C++ on macOS.
if (APPLE AND NOT DEFINED CMAKE_OSX_DEPLOYMENT_TARGET)
    set(CMAKE_OSX_DEPLOYMENT_TARGET 10.13 CACHE STRING "Minimum macOS version")
endif()

project(superduper-dsp-wrappers
    LANGUAGES C CXX
    VERSION 0.10.0
    DESCRIPTION "VST3/AUv2 wrappers around the SuperDuper DSP CLAP plugins"
)

# clap-wrapper uses C++17 features (nested namespace) and Apple's
# AudioUnitSDK 1.4.0 uses C++20 features (std::span, `requires`). Set
# 20 globally — VST3 SDK + clap-wrapper happily compile against 20 too.
# When clap-wrapper is a subdirectory the top-level
# `set(CMAKE_CXX_STANDARD 17)` from its own CMakeLists doesn't fire,
# so we set it here. Required, otherwise `-Werror,-Wc++17-extensions`
# blows up on every wrapper and AudioUnitSDK fails at `std::span`.
set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# clap-wrapper enables Objective-C / Objective-C++ when it's the top-level
# project; in our `add_subdirectory` mode we have to do it ourselves or
# the macOS build path fails with `CMAKE_OBJCXX_COMPILE_OBJECT not set`.
if (APPLE)
    enable_language(OBJC)
    enable_language(OBJCXX)
endif()

# Have clap-wrapper grab the SDKs it needs at configure time. The
# alternative is checking them in (~hundreds of MB) or making the user
# pin them manually — both worse than a one-time CPM fetch.
option(CLAP_WRAPPER_DOWNLOAD_DEPENDENCIES "Auto-download VST3 SDK + CLAP headers via CPM" ON)
option(SDSP_WRAPPERS_INSTALL_LOCAL "Copy built wrappers into ~/Library/Audio/Plug-Ins" OFF)
# AUv2 builds against AudioUnitSDK 1.4.0 + macOS 26 SDK when CMake is
# given C++23 (`std::span`, `std::expected`, `requires`) and the two
# `-Wno-error=…` patches in our clap-wrapper patches file are applied.
# Set ON on Apple, OFF elsewhere.
if (APPLE)
    option(CLAP_WRAPPER_BUILD_AUV2 "Also build AUv2 components alongside VST3" ON)
else()
    option(CLAP_WRAPPER_BUILD_AUV2 "Also build AUv2 components alongside VST3" OFF)
endif()

# Pull in clap-wrapper as a regular subdirectory. PROJECT_IS_TOP_LEVEL
# is false inside the subdirectory so its `top_level_default.cmake`
# (which would otherwise expect exactly one plugin) won't fire.
add_subdirectory(tools/clap-wrapper)

# Plugin manifest — see cmake/plugin_list.cmake for the table.
include(cmake/plugin_list.cmake)

# Helper that turns one row of the plugin manifest into VST3 (+ AUv2) targets.
function(sdsp_add_wrapper)
    set(oneValueArgs CRATE NAME CLAP_ID BUNDLE_ID AU_TYPE AU_SUBTYPE)
    cmake_parse_arguments(SDSP "" "${oneValueArgs}" "" ${ARGN})

    string(MAKE_C_IDENTIFIER "${SDSP_CRATE}" sdsp_id)

    # ---- VST3 ----
    add_library(${sdsp_id}_vst3 MODULE)
    target_add_vst3_wrapper(
        TARGET ${sdsp_id}_vst3
        OUTPUT_NAME "${SDSP_NAME}"
        SUPPORTS_ALL_NOTE_EXPRESSIONS FALSE
        BUNDLE_IDENTIFIER "${SDSP_BUNDLE_ID}.vst3"
        BUNDLE_VERSION "${PROJECT_VERSION}"
    )

    if (APPLE AND CLAP_WRAPPER_BUILD_AUV2)
        # ---- AUv2 ----
        add_library(${sdsp_id}_auv2 MODULE)
        target_add_auv2_wrapper(
            TARGET ${sdsp_id}_auv2
            OUTPUT_NAME "${SDSP_NAME}"
            BUNDLE_IDENTIFIER "${SDSP_BUNDLE_ID}.auv2"
            BUNDLE_VERSION "${PROJECT_VERSION}"
            INSTRUMENT_TYPE "${SDSP_AU_TYPE}"        # aufx for effects, aumu for instruments
            MANUFACTURER_NAME "SuperDuperAI"
            MANUFACTURER_CODE "SDsp"
            SUBTYPE_CODE "${SDSP_AU_SUBTYPE}"
        )
    endif()

    if (SDSP_WRAPPERS_INSTALL_LOCAL AND APPLE)
        set(vst3_dst "$ENV{HOME}/Library/Audio/Plug-Ins/VST3")
        add_custom_command(TARGET ${sdsp_id}_vst3 POST_BUILD
            COMMAND ${CMAKE_COMMAND} -E make_directory "${vst3_dst}"
            COMMAND ${CMAKE_COMMAND} -E copy_directory
                "$<TARGET_BUNDLE_DIR:${sdsp_id}_vst3>"
                "${vst3_dst}/${SDSP_NAME}.vst3"
        )
        if (CLAP_WRAPPER_BUILD_AUV2)
            set(au_dst "$ENV{HOME}/Library/Audio/Plug-Ins/Components")
            add_custom_command(TARGET ${sdsp_id}_auv2 POST_BUILD
                COMMAND ${CMAKE_COMMAND} -E make_directory "${au_dst}"
                COMMAND ${CMAKE_COMMAND} -E copy_directory
                    "$<TARGET_BUNDLE_DIR:${sdsp_id}_auv2>"
                    "${au_dst}/${SDSP_NAME}.component"
            )
        endif()
    endif()
endfunction()

foreach(row IN LISTS SDSP_WRAPPER_PLUGINS)
    string(REPLACE "|" ";" row_list "${row}")
    list(GET row_list 0 crate)
    list(GET row_list 1 name)
    list(GET row_list 2 clap_id)
    list(GET row_list 3 bundle_id)
    list(GET row_list 4 au_type)
    list(GET row_list 5 au_subtype)
    message(STATUS "sdsp: ${name}  →  VST3 + AUv2 (clap id ${clap_id}, AU ${au_type}:${au_subtype})")
    sdsp_add_wrapper(
        CRATE "${crate}"
        NAME "${name}"
        CLAP_ID "${clap_id}"
        BUNDLE_ID "${bundle_id}"
        AU_TYPE "${au_type}"
        AU_SUBTYPE "${au_subtype}"
    )
endforeach()
