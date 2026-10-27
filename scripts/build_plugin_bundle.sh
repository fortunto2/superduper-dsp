#!/usr/bin/env bash
# Build, bundle and install SuperDuper .clap plugins, one row of
# cmake/plugin_list.cmake each — the list the wrappers, the installer and the
# release workflow read too, so a new plugin is added there and nowhere else.
#
#   scripts/build_plugin_bundle.sh reverb               # build + bundle + install one
#   scripts/build_plugin_bundle.sh reverb denoise ...   # several (short or crate names)
#   scripts/build_plugin_bundle.sh --all                # every plugin in the list
#
#   --no-build        package what target/release already holds (cargo ran before)
#   --no-install      leave ~/Library/Audio/Plug-Ins/CLAP alone
#   --out DIR         where the .clap bundles go (default: dist)
#   --version V       CFBundle(Short)Version (default: .claude-plugin/plugin.json)
#
# Replaces 36 near-copies of build_<name>_bundle.sh (2026-10-27). Their
# Info.plist differed only in the display name and a stale 0.1.0/0.2.0;
# hosts read both from the CLAP descriptor in the binary, not from the plist.
# The plist written here is the release workflow's, so a local bundle is the
# shipped one.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

BUILD=1 INSTALL=1 OUT="dist" VERSION="" WANT=()
while (($#)); do
    case "$1" in
        --no-build) BUILD=0 ;;
        --no-install) INSTALL=0 ;;
        --out) OUT="$2"; shift ;;
        --version) VERSION="$2"; shift ;;
        --all) WANT+=("--all") ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) WANT+=("superduper-${1#superduper-}") ;;
    esac
    shift
done
((${#WANT[@]})) || { echo "usage: $0 <plugin>... | --all  (see --help)" >&2; exit 2; }
[[ -n "$VERSION" ]] || VERSION="$(sed -nE 's/.*"version": *"([^"]+)".*/\1/p' .claude-plugin/plugin.json | head -1)"

# crate|name|clap id|bundle id prefix|aux type|sub code
ROWS=()
while IFS= read -r row; do ROWS+=("$row"); done < <(
    grep -E '^\s+"superduper-' cmake/plugin_list.cmake | tr -d ' "')

SELECTED=()
for want in "${WANT[@]}"; do
    if [[ "$want" == "--all" ]]; then SELECTED+=("${ROWS[@]}"); continue; fi
    hit=""
    for row in "${ROWS[@]}"; do [[ "${row%%|*}" == "$want" ]] && hit="$row"; done
    [[ -n "$hit" ]] || { echo "ERROR: $want is not in cmake/plugin_list.cmake" >&2; exit 2; }
    SELECTED+=("$hit")
done

if ((BUILD)); then
    crates=()
    for row in "${SELECTED[@]}"; do crates+=(-p "${row%%|*}"); done
    echo "==> cargo build --release (${#SELECTED[@]} plugins)"
    cargo build --release "${crates[@]}"
fi

TARGET_ROOT="${CARGO_TARGET_DIR:-$ROOT/target}"
mkdir -p "$OUT"
for row in "${SELECTED[@]}"; do
    IFS='|' read -r crate name bundle_id _rest <<<"$row"
    dylib="$TARGET_ROOT/release/lib${crate//-/_}.dylib"
    [[ -f "$dylib" ]] || { echo "ERROR: $dylib not found (build first, or drop --no-build)" >&2; exit 1; }
    bundle="$OUT/$name.clap"
    rm -rf "$bundle"
    mkdir -p "$bundle/Contents/MacOS"
    cp "$dylib" "$bundle/Contents/MacOS/$name"
    cat > "$bundle/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>English</string>
    <key>CFBundleExecutable</key><string>$name</string>
    <key>CFBundleIdentifier</key><string>$bundle_id</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>$name</string>
    <key>CFBundlePackageType</key><string>BNDL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
</dict>
</plist>
EOF
    codesign --force --sign - --deep "$bundle" 2>/dev/null || true
    if ((INSTALL)); then
        dest="$HOME/Library/Audio/Plug-Ins/CLAP/$name.clap"
        mkdir -p "$(dirname "$dest")"
        rm -rf "$dest"
        cp -R "$bundle" "$dest"
        echo "  installed $dest"
    else
        echo "  $bundle"
    fi
done
