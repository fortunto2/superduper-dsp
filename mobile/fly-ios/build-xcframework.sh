#!/usr/bin/env bash
# Build the connectome simulation staticlib (device + simulator) and package it as an
# XCFramework that Flykeeper links. Run from anywhere; outputs into the flykeeper repo.
set -euo pipefail

CRATE_DIR="$(cd "$(dirname "$0")" && pwd)"
WS_DIR="$(cd "$CRATE_DIR/../.." && pwd)"
TGT="${CARGO_TARGET_DIR:-$WS_DIR/target}"
OUT="${1:-$HOME/startups/active/flykeeper/Vendor}"
INC="$CRATE_DIR/include"

echo "▸ cargo build (device + simulator)"
( cd "$WS_DIR" && cargo build -p fly-ios --release --target aarch64-apple-ios )
( cd "$WS_DIR" && cargo build -p fly-ios --release --target aarch64-apple-ios-sim )

echo "▸ assemble XCFramework → $OUT/FlyBrain.xcframework"
mkdir -p "$OUT"
rm -rf "$OUT/FlyBrain.xcframework"
xcodebuild -create-xcframework \
  -library "$TGT/aarch64-apple-ios/release/libfly_ios.a"     -headers "$INC" \
  -library "$TGT/aarch64-apple-ios-sim/release/libfly_ios.a" -headers "$INC" \
  -output  "$OUT/FlyBrain.xcframework"

echo "✓ done"
