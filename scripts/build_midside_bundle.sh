#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
cargo build --release -p superduper-midside
TARGET_ROOT="${CARGO_TARGET_DIR:-$ROOT/target}"
DYLIB="$TARGET_ROOT/release/libsuperduper_midside.dylib"
[[ -f "$DYLIB" ]] || { echo "ERROR: dylib not found"; exit 1; }
BUNDLE="$ROOT/dist/SuperDuperMidSide.clap"
mkdir -p "$ROOT/dist"; rm -rf "$BUNDLE"; mkdir -p "$BUNDLE/Contents/MacOS"
cp "$DYLIB" "$BUNDLE/Contents/MacOS/SuperDuperMidSide"
cat > "$BUNDLE/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>English</string>
    <key>CFBundleExecutable</key><string>SuperDuperMidSide</string>
    <key>CFBundleIdentifier</key><string>co.superduperai.midside</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>SuperDuper Mid/Side</string>
    <key>CFBundlePackageType</key><string>BNDL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>0.1.0</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
</dict>
</plist>
PLIST
codesign --force --sign - --deep "$BUNDLE" 2>/dev/null || true
DEST="$HOME/Library/Audio/Plug-Ins/CLAP/SuperDuperMidSide.clap"
mkdir -p "$(dirname "$DEST")"; rm -rf "$DEST"; cp -R "$BUNDLE" "$DEST"
echo "==> Installed $DEST"
