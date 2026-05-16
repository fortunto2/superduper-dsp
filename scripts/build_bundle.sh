#!/usr/bin/env bash
# Build SuperDuperDSP.clap bundle for macOS.
# Usage: ./scripts/build_bundle.sh [--release | --debug]
#
# Respects CARGO_TARGET_DIR if set (otherwise uses ./target).
# M0: bundles only the plugin dylib. Daemon ships in M3.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PROFILE="${1:-release}"
PROFILE="${PROFILE#--}"

echo "==> Building plugin ($PROFILE)..."
if [[ "$PROFILE" == "release" ]]; then
    cargo build --release -p superduper-dsp-plugin
    TARGET_SUBDIR="release"
else
    cargo build -p superduper-dsp-plugin
    TARGET_SUBDIR="debug"
fi

# Resolve target directory (CARGO_TARGET_DIR > ./target).
TARGET_ROOT="${CARGO_TARGET_DIR:-$ROOT/target}"
TARGET_DIR="$TARGET_ROOT/$TARGET_SUBDIR"
DYLIB="$TARGET_DIR/libsuperduper_dsp.dylib"

if [[ ! -f "$DYLIB" ]]; then
    echo "ERROR: plugin dylib not found at $DYLIB"
    exit 1
fi

# Bundle goes next to the source workspace so it's easy to find regardless of
# where CARGO_TARGET_DIR points.
BUNDLE="$ROOT/dist/SuperDuperDSP.clap"
echo "==> Assembling bundle at $BUNDLE..."
mkdir -p "$ROOT/dist"
rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS"

# Plugin binary inside bundle must match CFBundleExecutable.
cp "$DYLIB" "$BUNDLE/Contents/MacOS/SuperDuperDSP"

cat > "$BUNDLE/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>English</string>
    <key>CFBundleExecutable</key>
    <string>SuperDuperDSP</string>
    <key>CFBundleIdentifier</key>
    <string>co.superduperai.dsp</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>SuperDuper DSP</string>
    <key>CFBundlePackageType</key>
    <string>BNDL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleSignature</key>
    <string>????</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
</dict>
</plist>
PLIST

echo "==> Bundle ready: $BUNDLE"
echo "    Install with: ./scripts/install_local.sh"
