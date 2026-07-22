#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
echo "==> Building superduper-vocoder (release)..."
cargo build --release -p superduper-vocoder
TARGET_ROOT="${CARGO_TARGET_DIR:-$ROOT/target}"
DYLIB="$TARGET_ROOT/release/libsuperduper_vocoder.dylib"
[[ -f "$DYLIB" ]] || { echo "ERROR: dylib not found at $DYLIB"; exit 1; }
BUNDLE="$ROOT/dist/SuperDuperVocoder.clap"
mkdir -p "$ROOT/dist"; rm -rf "$BUNDLE"; mkdir -p "$BUNDLE/Contents/MacOS"
cp "$DYLIB" "$BUNDLE/Contents/MacOS/SuperDuperVocoder"
cat > "$BUNDLE/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>English</string>
    <key>CFBundleExecutable</key><string>SuperDuperVocoder</string>
    <key>CFBundleIdentifier</key><string>co.superduperai.vocoder</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>SuperDuper Vocoder</string>
    <key>CFBundlePackageType</key><string>BNDL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>0.1.0</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
</dict>
</plist>
PLIST
DEST="$HOME/Library/Audio/Plug-Ins/CLAP/SuperDuperVocoder.clap"
mkdir -p "$(dirname "$DEST")"; rm -rf "$DEST"; cp -R "$BUNDLE" "$DEST"
echo "==> Installed to $DEST"
