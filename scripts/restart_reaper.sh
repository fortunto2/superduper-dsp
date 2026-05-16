#!/usr/bin/env bash
# Quit REAPER and re-open it so the SuperDuper DSP plugin dylib gets reloaded.
#
# Why this exists: macOS DAWs cache plugin .dylibs in their process address
# space. Replacing the .clap on disk doesn't make REAPER pick up the new
# binary — you have to fully restart the host. This script automates the
# Cmd+Q + open dance.
#
# Usage:
#   ./scripts/restart_reaper.sh           # graceful quit (saves prompt) + open
#   ./scripts/restart_reaper.sh --force   # SIGKILL + open (no save prompt)

set -euo pipefail

FORCE=0
if [[ "${1:-}" == "--force" ]]; then
    FORCE=1
fi

if pgrep -x "REAPER" > /dev/null; then
    if [[ "$FORCE" == "1" ]]; then
        echo "==> Killing REAPER (forced)..."
        pkill -9 -x REAPER || true
    else
        echo "==> Asking REAPER to quit (may prompt to save)..."
        osascript -e 'tell application "REAPER" to quit' 2>/dev/null || true
    fi
    # Wait up to 6 seconds for the process to actually exit.
    for _ in 1 2 3 4 5 6; do
        sleep 1
        if ! pgrep -x "REAPER" > /dev/null; then
            break
        fi
    done
    if pgrep -x "REAPER" > /dev/null; then
        echo "    REAPER still running. Re-run with --force or quit manually."
        exit 1
    fi
fi

echo "==> Launching REAPER..."
open -a REAPER

echo "==> Done. Plugin dylib in ~/Library/Audio/Plug-Ins/CLAP/SuperDuperDSP.clap"
echo "    will be re-loaded from disk on next plugin instantiation."
