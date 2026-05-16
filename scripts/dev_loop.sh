#!/usr/bin/env bash
# Watch source files and rebuild+install the .clap bundle on every change.
# Useful while iterating on plugin/daemon source.
# Requires cargo-watch:  cargo install cargo-watch

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if ! command -v cargo-watch >/dev/null; then
    echo "Install cargo-watch first: cargo install cargo-watch"
    exit 1
fi

# Note: REAPER must be restarted to pick up new .clap. There is no hot-reload
# for the plugin itself (only for the user effect dylibs it loads at runtime).
# Run this in one terminal, restart REAPER manually when needed.
cargo watch -x 'build --release -p superduper-dsp-plugin -p superduper-dsp-daemon' \
    -s "$ROOT/scripts/build_bundle.sh --release" \
    -s "$ROOT/scripts/install_local.sh"
