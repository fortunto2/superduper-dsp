#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
cargo build --release -p superduper-wave
TARGET_ROOT="${CARGO_TARGET_DIR:-$ROOT/target}"
DYLIB="$TARGET_ROOT/release/libsuperduper_wave.dylib"
[[ -f "$DYLIB" ]] || { echo "ERROR: dylib not found"; exit 1; }
BUNDLE="$ROOT/dist/SuperDuperWave.clap"
mkdir -p "$ROOT/dist"; rm -rf "$BUNDLE"; mkdir -p "$BUNDLE/Contents/MacOS"
cp "$DYLIB" "$BUNDLE/Contents/MacOS/SuperDuperWave"
cat > "$BUNDLE/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>English</string>
    <key>CFBundleExecutable</key><string>SuperDuperWave</string>
    <key>CFBundleIdentifier</key><string>co.superduperai.wave</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>SuperDuper Wave</string>
    <key>CFBundlePackageType</key><string>BNDL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>0.1.0</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
</dict>
</plist>
PLIST
codesign --force --sign - --deep "$BUNDLE" 2>/dev/null || true
DEST="$HOME/Library/Audio/Plug-Ins/CLAP/SuperDuperWave.clap"
mkdir -p "$(dirname "$DEST")"; rm -rf "$DEST"; cp -R "$BUNDLE" "$DEST"
echo "==> Installed $DEST"
