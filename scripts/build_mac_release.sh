#!/usr/bin/env bash
# Local macOS release build for v$V with the same asset names as release.yml.
set -euo pipefail
V="$1"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
OUT="$ROOT/dist/release-mac-$V"; rm -rf "$OUT"; mkdir -p "$OUT/macos" "$OUT/vst3" "$OUT/au"
T="${CARGO_TARGET_DIR:-$ROOT/target}"
cargo build --release --workspace --exclude superduper-denoise --lib 2>&1 | tail -1
grep -E '^\s+"superduper-' cmake/plugin_list.cmake | tr -d ' "' | while IFS='|' read -r crate name clap_id _rest; do
  stem="${crate//-/_}"
  dylib="$T/release/lib${stem}.dylib"
  [[ -f "$dylib" ]] || { echo "missing $dylib"; exit 1; }
  b="$OUT/macos/$name.clap"; mkdir -p "$b/Contents/MacOS"; cp "$dylib" "$b/Contents/MacOS/$name"
  cat > "$b/Contents/Info.plist" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleDevelopmentRegion</key><string>English</string>
<key>CFBundleExecutable</key><string>$name</string>
<key>CFBundleIdentifier</key><string>$clap_id</string>
<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
<key>CFBundleName</key><string>$name</string>
<key>CFBundlePackageType</key><string>BNDL</string>
<key>CFBundleShortVersionString</key><string>$V</string>
<key>CFBundleVersion</key><string>$V</string>
<key>LSMinimumSystemVersion</key><string>12.0</string>
</dict></plist>
PL
  codesign --force --sign - --deep "$b" 2>/dev/null || true
  (cd "$OUT/macos" && zip -qry "../${name}-${V}-macos-arm64.zip" "${name}.clap")
done
(cd "$OUT/macos" && zip -qry "../superduper-dsp-${V}-macos-arm64.zip" SuperDuper*.clap)
(cd "$OUT" && shasum -a 256 *.zip > SHA256SUMS-macos.txt)
# Wrappers load the .clap from ~/Library — install this build there first.
mkdir -p ~/Library/Audio/Plug-Ins/CLAP; for b in "$OUT"/macos/*.clap; do rm -rf ~/Library/Audio/Plug-Ins/CLAP/$(basename "$b"); cp -R "$b" ~/Library/Audio/Plug-Ins/CLAP/; done
PATH=/opt/homebrew/bin:$PATH bash scripts/build_wrappers.sh 2>&1 | tail -2
for v in build-wrappers/SuperDuper*.vst3; do n=$(basename "$v" .vst3); cp -R "$v" "$OUT/vst3/"; codesign --force --sign - --deep "$OUT/vst3/$n.vst3" 2>/dev/null || true; (cd "$OUT/vst3" && zip -qry "../${n}-${V}-macos-arm64-vst3.zip" "$n.vst3"); done
for a in build-wrappers/SuperDuper*.component; do n=$(basename "$a" .component); cp -R "$a" "$OUT/au/"; codesign --force --sign - --deep "$OUT/au/$n.component" 2>/dev/null || true; (cd "$OUT/au" && zip -qry "../${n}-${V}-macos-arm64-au.zip" "$n.component"); done
(cd "$OUT/vst3" && zip -qry "../superduper-dsp-${V}-macos-arm64-vst3.zip" SuperDuper*.vst3)
(cd "$OUT/au" && zip -qry "../superduper-dsp-${V}-macos-arm64-au.zip" SuperDuper*.component)
(cd "$OUT" && shasum -a 256 *vst3*.zip *au*.zip >> SHA256SUMS-macos.txt)
# Agent tools for the installer: the bus gateway, the chain renderer and the skills
# a user (not a plugin developer) needs. Each tool built ALONE — a workspace build
# unifies features and once hid a synth-core that only compiled with `gui` on.
cargo build --release -p sdsp-bus 2>&1 | tail -1
cargo build --release -p sdsp-chain 2>&1 | tail -1
mkdir -p "$OUT/agent/bin" "$OUT/agent/skills"
cp "$T/release/sdsp-bus" "$T/release/sdsp-chain" "$OUT/agent/bin/"
for b in "$OUT"/agent/bin/*; do codesign --force --sign - "$b" 2>/dev/null || true; done
for s in sdsp-mix superduper-song sdsp-chain audio-to-midi; do ditto skills/$s "$OUT/agent/skills/$s"; done
SDSP_PKG_DIST="$OUT" bash installer/macos/build_pkg.sh "$V" | tail -1
cp "$OUT/SuperDuperDSP-${V}-macos-arm64.pkg" "$OUT/SuperDuperDSP-macos-arm64.pkg"   # stable name for releases/latest/download
(cd "$OUT" && shasum -a 256 *.pkg >> SHA256SUMS-macos.txt)
ls "$OUT"/*.zip | wc -l
