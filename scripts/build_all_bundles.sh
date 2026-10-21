#!/usr/bin/env bash
# Rebuild and install every SuperDuper .clap bundle. Faster than calling
# each per-plugin script in series because cargo compiles the workspace once.
#
# The plugin list is cmake/plugin_list.cmake, the one the VST3/AU wrappers use.
# A list typed out here once stopped at 13 of 31 plugins, so an "install
# everything" left Drum, Piano and 16 more on their old build.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PLUGINS=()
while IFS= read -r crate; do PLUGINS+=("${crate#superduper-}"); done < <(
    grep -E '^\s+"superduper-' cmake/plugin_list.cmake | tr -d ' "' | cut -d'|' -f1)

echo "==> Compiling ${#PLUGINS[@]} plugins (release)..."
cargo build --release $(printf -- '-p superduper-%s ' "${PLUGINS[@]}")

echo "==> Packaging bundles..."
for n in "${PLUGINS[@]}"; do
    echo "  - $n"
    bash "scripts/build_${n}_bundle.sh" >/dev/null
done
echo "==> All bundles installed in ~/Library/Audio/Plug-Ins/CLAP/"
