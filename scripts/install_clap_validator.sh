#!/usr/bin/env bash
# Install clap-validator with our patches on top (tools/clap-validator-patches/).
#
#   scripts/install_clap_validator.sh            # clones into a temp dir, builds, installs to ~/.cargo/bin
#   scripts/install_clap_validator.sh --keep DIR # clone into DIR and keep it (to read the tests)
#
# `cargo install --git` keeps no checkout and takes no patches, so this script
# is the one way the installed binary matches what tools/validate_claps.py
# expects. Patches so far: process-audio-denormals sends the same note / MIDI
# events in both passes (a looper that records on a CC otherwise does
# different work in the two passes and the "denormal" ratio measures that).
# rustc >= 1.95 (0.4.x); `--tag 0.3.2` for 1.93.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR=""
if [[ "${1:-}" == "--keep" ]]; then DIR="${2:?dir}"; fi
if [[ -z "$DIR" ]]; then DIR="$(mktemp -d)/clap-validator"; fi
if [[ ! -d "$DIR/.git" ]]; then
    git clone -q --depth 1 https://github.com/free-audio/clap-validator.git "$DIR"
fi
cd "$DIR"
for p in "$ROOT"/tools/clap-validator-patches/*.patch; do
    [[ -f "$p" ]] || continue
    if git apply --check "$p" 2>/dev/null; then
        echo "==> applying $(basename "$p")"
        git apply "$p"
    else
        echo "==> $(basename "$p") already applied or does not apply (skipping)"
    fi
done
cargo install --path . --locked --force
clap-validator --version
