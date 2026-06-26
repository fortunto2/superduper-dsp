#!/usr/bin/env bash
# Build the SuperDuper iOS synth staticlib (device + simulator) and package it as an
# XCFramework that live2play links. Run from anywhere; outputs into the reelcam repo.
set -euo pipefail

CRATE_DIR="$(cd "$(dirname "$0")" && pwd)"
WS_DIR="$(cd "$CRATE_DIR/../.." && pwd)"
TGT="${CARGO_TARGET_DIR:-$WS_DIR/target}"
OUT="${1:-$HOME/startups/active/reelcam/Vendor}"
INC="$CRATE_DIR/include"

echo "▸ cargo build (device + simulator)"
( cd "$WS_DIR" && cargo build -p sdsp-ios --release --target aarch64-apple-ios )
( cd "$WS_DIR" && cargo build -p sdsp-ios --release --target aarch64-apple-ios-sim )

echo "▸ assemble XCFramework → $OUT/SDSP.xcframework"
mkdir -p "$OUT"
rm -rf "$OUT/SDSP.xcframework"
xcodebuild -create-xcframework \
  -library "$TGT/aarch64-apple-ios/release/libsdsp_ios.a"     -headers "$INC" \
  -library "$TGT/aarch64-apple-ios-sim/release/libsdsp_ios.a" -headers "$INC" \
  -output  "$OUT/SDSP.xcframework"

echo "✓ done"
