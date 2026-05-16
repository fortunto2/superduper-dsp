#!/usr/bin/env bash
# Copy a built effect dylib into the most recent SuperDuper DSP instance dir,
# triggering hot-reload via the plugin's file watcher.
#
# Usage:
#   ./scripts/load_effect.sh [effect-crate-name]
#
# Defaults to `example-passthrough`. Build it first:
#   cargo build --release -p example-passthrough

set -euo pipefail

EFFECT="${1:-example-passthrough}"
# Cargo replaces hyphens with underscores in the dylib name.
DYLIB_NAME="lib${EFFECT//-/_}.dylib"
TARGET_ROOT="${CARGO_TARGET_DIR:-$(cd "$(dirname "$0")/.." && pwd)/target}"
SRC="$TARGET_ROOT/release/$DYLIB_NAME"

if [[ ! -f "$SRC" ]]; then
    echo "ERROR: $SRC not found"
    echo "Build it first: cargo build --release -p $EFFECT"
    exit 1
fi

INSTANCES_DIR="$HOME/.superduper-dsp/instances"
if [[ ! -d "$INSTANCES_DIR" ]]; then
    echo "ERROR: $INSTANCES_DIR doesn't exist."
    echo "Load SuperDuper DSP in REAPER at least once first — it creates"
    echo "the directory on plugin instantiation."
    exit 1
fi

# Pick the most recently modified instance directory.
LATEST=$(ls -td "$INSTANCES_DIR"/*/ 2>/dev/null | head -n1 || true)
if [[ -z "$LATEST" ]]; then
    echo "ERROR: no instance directories under $INSTANCES_DIR"
    echo "Load SuperDuper DSP in REAPER first."
    exit 1
fi

DEST="$LATEST/effect.dylib"
echo "==> $SRC"
echo "==> $DEST"
cp "$SRC" "$DEST"
echo "==> Plugin watcher will pick it up within ~250ms."
echo "    Check REAPER audio: passthrough effect applied before gain."
