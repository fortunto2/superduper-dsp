#!/bin/bash
# Build VST3 + AUv2 wrappers around the Rust CLAP plugins.
#
# Prerequisites: cargo, cmake ≥ 3.21, the .clap bundles already built
# (run ./scripts/build_release.sh first or per-plugin build_*_bundle.sh).
#
# Usage:
#   ./scripts/build_wrappers.sh              # configure + build
#   ./scripts/build_wrappers.sh --install    # also copy to ~/Library/Audio/Plug-Ins
#   ./scripts/build_wrappers.sh --clean      # nuke build/ and rebuild from scratch

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

BUILD_DIR="${ROOT}/build-wrappers"
INSTALL_LOCAL=OFF
CLEAN=0

for arg in "$@"; do
    case "$arg" in
        --install) INSTALL_LOCAL=ON ;;
        --clean)   CLEAN=1 ;;
        -h|--help)
            sed -n '2,12p' "$0"
            exit 0
            ;;
        *) echo "unknown arg: $arg" >&2 ; exit 1 ;;
    esac
done

if [ "$CLEAN" = "1" ] && [ -d "$BUILD_DIR" ]; then
    echo "==> wiping ${BUILD_DIR}"
    rm -rf "$BUILD_DIR"
fi

# Refresh the submodule pointer first (in case the user just cloned).
if [ ! -f "${ROOT}/tools/clap-wrapper/CMakeLists.txt" ]; then
    echo "==> initialising clap-wrapper submodule"
    git -C "$ROOT" submodule update --init --recursive tools/clap-wrapper
fi

# Apply local clap-wrapper patches (macOS 26 SDK compatibility, VST3 SDK
# pin to 3.7.6, AudioUnitSDK to 1.4.0). Idempotent — we re-apply only
# if `git apply --check` says the patch hasn't landed yet.
PATCH_DIR="${ROOT}/tools/clap-wrapper-patches"
if [ -d "$PATCH_DIR" ]; then
    for patch in "$PATCH_DIR"/*.patch; do
        [ -f "$patch" ] || continue
        if (cd "$ROOT/tools/clap-wrapper" && git apply --check "$patch") 2>/dev/null; then
            echo "==> applying $(basename "$patch")"
            (cd "$ROOT/tools/clap-wrapper" && git apply "$patch")
        else
            echo "==> $(basename "$patch") already applied (skipping)"
        fi
    done
fi

# macOS 26 SDK ships with a libc++ that's stricter about atomic copy
# semantics + missing template specialisations in AudioUnitSDK. Prefer
# the 15.x SDK if installed alongside.
SYSROOT_FLAG=""
if [ -d "/Library/Developer/CommandLineTools/SDKs/MacOSX15.4.sdk" ]; then
    SYSROOT_FLAG="-DCMAKE_OSX_SYSROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX15.4.sdk"
fi

echo "==> configuring CMake in ${BUILD_DIR}"
cmake -B "$BUILD_DIR" -S "$ROOT" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCLAP_WRAPPER_DOWNLOAD_DEPENDENCIES=ON \
    -DSDSP_WRAPPERS_INSTALL_LOCAL="$INSTALL_LOCAL" \
    ${SYSROOT_FLAG}

echo "==> building wrappers"
cmake --build "$BUILD_DIR" --config Release --parallel

echo
echo "==> built wrappers:"
find "$BUILD_DIR" -maxdepth 4 \( -name "*.vst3" -o -name "*.component" \) -type d 2>/dev/null | sort

if [ "$INSTALL_LOCAL" = "ON" ]; then
    echo
    echo "==> installed copies:"
    find ~/Library/Audio/Plug-Ins/VST3 ~/Library/Audio/Plug-Ins/Components \
        -maxdepth 1 -name "SuperDuper*" 2>/dev/null | sort
fi
