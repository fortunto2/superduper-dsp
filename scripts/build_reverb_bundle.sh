#!/usr/bin/env bash
# Build and install SuperDuper Reverb as a standalone .clap bundle.
#
# Result: ~/Library/Audio/Plug-Ins/CLAP/SuperDuperReverb.clap shows up as a
# separate plugin in REAPER's FX browser — independent of SuperDuper DSP.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "==> Building superduper-reverb (release)..."
cargo build --release -p superduper-reverb

TARGET_ROOT="${CARGO_TARGET_DIR:-$ROOT/target}"
DYLIB="$TARGET_ROOT/release/libsuperduper_reverb.dylib"
if [[ ! -f "$DYLIB" ]]; then
    echo "ERROR: dylib not found at $DYLIB"
    exit 1
fi

BUNDLE="$ROOT/dist/SuperDuperReverb.clap"
echo "==> Assembling bundle at $BUNDLE..."
mkdir -p "$ROOT/dist"
rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS"
cp "$DYLIB" "$BUNDLE/Contents/MacOS/SuperDuperReverb"

cat > "$BUNDLE/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>English</string>
    <key>CFBundleExecutable</key>
    <string>SuperDuperReverb</string>
    <key>CFBundleIdentifier</key>
    <string>co.superduperai.reverb</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>SuperDuper Reverb</string>
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

DEST="$HOME/Library/Audio/Plug-Ins/CLAP/SuperDuperReverb.clap"
echo "==> Installing to $DEST..."
mkdir -p "$(dirname "$DEST")"
rm -rf "$DEST"
cp -R "$BUNDLE" "$DEST"

echo "==> Done. Restart REAPER and find 'SuperDuper Reverb' in the FX browser."
