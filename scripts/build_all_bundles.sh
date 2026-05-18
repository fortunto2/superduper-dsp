#!/usr/bin/env bash
# Rebuild and install every SuperDuper .clap bundle. Faster than calling
# each per-plugin script in series because cargo compiles the workspace once.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "==> Compiling whole workspace (release)..."
cargo build --release \
    -p superduper-reverb \
    -p superduper-supermass \
    -p superduper-spectrum \
    -p superduper-saturator \
    -p superduper-delay \
    -p superduper-compressor \
    -p superduper-eq \
    -p superduper-limiter \
    -p superduper-ambient \
    -p superduper-pad \
    -p superduper-vocal \
    -p superduper-wave \
    -p superduper-kubyz

echo "==> Packaging bundles..."
for s in build_reverb_bundle.sh build_supermass_bundle.sh build_spectrum_bundle.sh \
         build_saturator_bundle.sh build_delay_bundle.sh build_compressor_bundle.sh \
         build_eq_bundle.sh build_limiter_bundle.sh build_ambient_bundle.sh \
         build_pad_bundle.sh build_vocal_bundle.sh build_wave_bundle.sh \
         build_kubyz_bundle.sh; do
    if [[ -x "scripts/$s" ]]; then
        echo "  - $s"
        bash "scripts/$s" >/dev/null
    fi
done
echo "==> All bundles installed in ~/Library/Audio/Plug-Ins/CLAP/"
