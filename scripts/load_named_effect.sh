#!/usr/bin/env bash
# Switch the active SuperDuper DSP effect to one of the previously-compiled
# effects in ~/.superduper-dsp/effect-builds/.
#
# Usage:
#   ./scripts/load_named_effect.sh                 # lists what's available
#   ./scripts/load_named_effect.sh ua_610b_eq      # makes it the active dylib

set -euo pipefail

BUILDS="$HOME/.superduper-dsp/effect-builds"
INSTANCES="$HOME/.superduper-dsp/instances"

if [[ ! -d "$BUILDS" ]]; then
    echo "ERROR: $BUILDS doesn't exist. Run a build first."
    exit 1
fi

if [[ $# -lt 1 ]]; then
    echo "Available effects:"
    set +e
    for dir in "$BUILDS"/*/; do
        [[ -d "$dir" ]] || continue
        name=$(basename "$dir")
        src="$dir/src/lib.rs"
        if [[ -f "$src" ]]; then
            params=$(awk '/^params! \{/,/^}/' "$src" \
                | grep -E '^[[:space:]]+[A-Z_]+[[:space:]]+=' \
                | tr -s '[:space:]' ' ' \
                | sed 's/^ //; s/, $//')
            printf "  %-18s %s\n" "$name" "${params:0:120}"
        fi
    done
    set -e
    echo
    echo "Usage: $0 <effect-name>"
    exit 0
fi

NAME="$1"
SRC="$BUILDS/$NAME/target/aarch64-apple-darwin/release/libeffect_${NAME}.dylib"
if [[ ! -f "$SRC" ]]; then
    SRC="$BUILDS/$NAME/target/x86_64-apple-darwin/release/libeffect_${NAME}.dylib"
fi
if [[ ! -f "$SRC" ]]; then
    echo "ERROR: $NAME isn't built. Try:"
    echo "  cargo build --release --target aarch64-apple-darwin --manifest-path $BUILDS/$NAME/Cargo.toml"
    exit 1
fi

# Find primary instance from plugin.log so we hit the directory the plugin
# actually has cached.
LOG="$HOME/.superduper-dsp/plugin.log"
UUID=""
if [[ -f "$LOG" ]]; then
    UUID=$(grep "bound primary" "$LOG" | tail -n1 | awk '{print $NF}' || true)
fi
if [[ -z "$UUID" || ! -d "$INSTANCES/$UUID" ]]; then
    # Fallback to most recent instance dir.
    if [[ -d "$INSTANCES" ]]; then
        TARGET_DIR=$(ls -td "$INSTANCES"/*/ 2>/dev/null | head -n1 || true)
        TARGET_DIR="${TARGET_DIR%/}"
    fi
else
    TARGET_DIR="$INSTANCES/$UUID"
fi

if [[ -z "${TARGET_DIR:-}" ]]; then
    echo "ERROR: cannot locate a live plugin instance directory."
    echo "Load SuperDuper DSP onto a REAPER track first."
    exit 1
fi

mkdir -p "$TARGET_DIR"
DST="$TARGET_DIR/effect.dylib"
echo "==> $NAME"
echo "==> $SRC"
echo "==> $DST"
cp "$SRC" "$DST"
touch "$DST"
echo "==> Watcher will swap within ~250ms; REAPER will rescan params automatically."
