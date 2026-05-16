#!/usr/bin/env bash
# Install SuperDuperDSP.clap to ~/Library/Audio/Plug-Ins/CLAP/

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUNDLE="$ROOT/dist/SuperDuperDSP.clap"
DEST="$HOME/Library/Audio/Plug-Ins/CLAP"

if [[ ! -d "$BUNDLE" ]]; then
    echo "ERROR: bundle not found at $BUNDLE"
    echo "Run ./scripts/build_bundle.sh first"
    exit 1
fi

mkdir -p "$DEST"
rm -rf "$DEST/SuperDuperDSP.clap"
cp -R "$BUNDLE" "$DEST/"

echo "==> Installed: $DEST/SuperDuperDSP.clap"
echo "    Restart REAPER and find 'SuperDuper DSP' in the FX browser."
