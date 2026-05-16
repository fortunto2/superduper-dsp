#!/usr/bin/env bash
# Copy a built effect dylib into the active SuperDuper DSP instance's directory,
# triggering hot-reload via the plugin's file watcher (or arming the Reload
# toggle).
#
# Discovery order for the target directory:
#   1. Most recent `new_shared: instance <uuid> ready` line in plugin.log,
#      because that path matches what the plugin itself has cached in
#      `PluginShared::effect_dylib_path` and will look for on Reload.
#   2. Most recently modified subdirectory of ~/.superduper-dsp/instances/
#      (fallback when plugin.log was wiped).
#
# Usage:
#   ./scripts/load_effect.sh [effect-crate-name]
#
# Default crate: `example-passthrough`.
# Build the effect first: `cargo build --release -p <effect-crate-name>`.

set -euo pipefail

EFFECT="${1:-example-passthrough}"
DYLIB_NAME="lib${EFFECT//-/_}.dylib"
TARGET_ROOT="${CARGO_TARGET_DIR:-$(cd "$(dirname "$0")/.." && pwd)/target}"
SRC="$TARGET_ROOT/release/$DYLIB_NAME"

if [[ ! -f "$SRC" ]]; then
    echo "ERROR: $SRC not found"
    echo "Build it first: cargo build --release -p $EFFECT"
    exit 1
fi

ROOT_DIR="$HOME/.superduper-dsp"
INSTANCES_DIR="$ROOT_DIR/instances"
LOG="$ROOT_DIR/plugin.log"

# 1) Read latest instance UUID from plugin.log if available.
TARGET_DIR=""
if [[ -f "$LOG" ]]; then
    UUID=$(grep "new_shared: instance" "$LOG" | tail -n1 | awk '{print $4}')
    if [[ -n "${UUID:-}" ]]; then
        TARGET_DIR="$INSTANCES_DIR/$UUID"
    fi
fi

# 2) Fallback: latest subdirectory.
if [[ -z "$TARGET_DIR" || ! -d "$TARGET_DIR" ]]; then
    if [[ -d "$INSTANCES_DIR" ]]; then
        FALLBACK=$(ls -td "$INSTANCES_DIR"/*/ 2>/dev/null | head -n1 || true)
        if [[ -n "${FALLBACK:-}" ]]; then
            TARGET_DIR="${FALLBACK%/}"
        fi
    fi
fi

if [[ -z "$TARGET_DIR" ]]; then
    echo "ERROR: cannot locate a SuperDuper DSP instance directory."
    echo "Make sure the plugin has been loaded at least once (it creates"
    echo "the per-instance dir on PluginShared::new())."
    exit 1
fi

# Plugin may have lost the directory (e.g. someone did rm -rf). Re-create it —
# the plugin's cached path still works as long as the file is there.
mkdir -p "$TARGET_DIR"

DEST="$TARGET_DIR/effect.dylib"
echo "==> src : $SRC"
echo "==> dst : $DEST"
cp "$SRC" "$DEST"
# Force an mtime bump so FSEvents emits a Modify event even when the source
# was identical.
touch "$DEST"
echo "==> Plugin watcher will pick it up within ~250ms."
echo "    (Or press the Reload toggle in the plugin UI to force-swap now.)"
