#!/usr/bin/env bash
# Rebuild and install every SuperDuper .clap bundle in cmake/plugin_list.cmake,
# the list the VST3/AU wrappers, the installer and the release workflow read.
# A list typed out here once stopped at 13 of 31 plugins, so an "install
# everything" left Drum, Piano and 16 more on their old build.
set -euo pipefail
exec "$(dirname "$0")/build_plugin_bundle.sh" --all "$@"
