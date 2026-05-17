#!/usr/bin/env bash
# Build all SuperDuper effect plugins, package them as .clap bundles, zip
# them up, and produce a release directory ready to upload to GitHub.
#
# Usage:
#   ./scripts/build_release.sh 0.1.0
#
# Output:
#   dist/release-0.1.0/
#       SuperDuperReverb-0.1.0-macos-arm64.zip
#       SuperDuperSupermass-0.1.0-macos-arm64.zip
#       SuperDuperSpectrum-0.1.0-macos-arm64.zip
#       superduper-dsp-0.1.0-macos-arm64.zip      (all three together)
#       SHA256SUMS

set -euo pipefail

VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
    echo "Usage: $0 <version>   e.g. $0 0.1.0"
    exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PLATFORM="macos-arm64"
RELEASE_DIR="$ROOT/dist/release-$VERSION"
BUNDLE_TMP="$ROOT/dist/_bundles"

rm -rf "$RELEASE_DIR" "$BUNDLE_TMP"
mkdir -p "$RELEASE_DIR" "$BUNDLE_TMP"

# ---------------------------------------------------------------------------
# Plugin manifest: (crate_name, dylib_name, bundle_name, identifier).
# Add a new entry when shipping a new effect.
# ---------------------------------------------------------------------------
PLUGINS=(
    "superduper-reverb|libsuperduper_reverb.dylib|SuperDuperReverb|co.superduperai.reverb"
    "superduper-supermass|libsuperduper_supermass.dylib|SuperDuperSupermass|co.superduperai.supermass"
    "superduper-spectrum|libsuperduper_spectrum.dylib|SuperDuperSpectrum|co.superduperai.spectrum"
    "superduper-saturator|libsuperduper_saturator.dylib|SuperDuperSaturator|co.superduperai.saturator"
    "superduper-delay|libsuperduper_delay.dylib|SuperDuperDelay|co.superduperai.delay"
    "superduper-compressor|libsuperduper_compressor.dylib|SuperDuperCompressor|co.superduperai.compressor"
    "superduper-eq|libsuperduper_eq.dylib|SuperDuperEq|co.superduperai.eq"
    "superduper-limiter|libsuperduper_limiter.dylib|SuperDuperLimiter|co.superduperai.limiter"
    "superduper-ambient|libsuperduper_ambient.dylib|SuperDuperAmbient|co.superduperai.ambient"
    "superduper-pad|libsuperduper_pad.dylib|SuperDuperPad|co.superduperai.pad"
    "superduper-vocal|libsuperduper_vocal.dylib|SuperDuperVocal|co.superduperai.vocal"
)

echo "==> Building release binaries..."
TARGET_ROOT="${CARGO_TARGET_DIR:-$ROOT/target}"
# Build all nine crates in one go so cargo amortises the shared deps.
cargo build --release \
    -p superduper-reverb \
    -p superduper-supermass \
    -p superduper-spectrum \
    -p superduper-saturator \
    -p superduper-delay \
    -p superduper-compressor \
    -p superduper-eq \
    -p superduper-limiter \
    -p superduper-ambient \
    -p superduper-pad \
    -p superduper-vocal

# ---------------------------------------------------------------------------
# Bundle each plugin as a .clap (macOS bundle = directory).
# ---------------------------------------------------------------------------
for entry in "${PLUGINS[@]}"; do
    IFS='|' read -r crate dylib bundle_name bundle_id <<<"$entry"
    DYLIB_PATH="$TARGET_ROOT/release/$dylib"
    [[ -f "$DYLIB_PATH" ]] || { echo "ERROR: $DYLIB_PATH missing"; exit 1; }

    BUNDLE_DIR="$BUNDLE_TMP/${bundle_name}.clap"
    mkdir -p "$BUNDLE_DIR/Contents/MacOS"
    cp "$DYLIB_PATH" "$BUNDLE_DIR/Contents/MacOS/$bundle_name"

    cat > "$BUNDLE_DIR/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>English</string>
    <key>CFBundleExecutable</key><string>$bundle_name</string>
    <key>CFBundleIdentifier</key><string>$bundle_id</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>$bundle_name</string>
    <key>CFBundlePackageType</key><string>BNDL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
</dict>
</plist>
EOF

    # Ad-hoc sign (no Developer ID needed). This won't bypass Gatekeeper but
    # makes the bundle look "intentional" to macOS so users only see the
    # quarantine prompt, not "damaged".
    codesign --force --sign - --deep "$BUNDLE_DIR" 2>/dev/null || true

    # Single-plugin zip — let users grab just one effect if they want.
    INDIVIDUAL_ZIP="$RELEASE_DIR/${bundle_name}-${VERSION}-${PLATFORM}.zip"
    (cd "$BUNDLE_TMP" && zip -qry "$INDIVIDUAL_ZIP" "${bundle_name}.clap")
    echo "==> Packaged $INDIVIDUAL_ZIP"
done

# ---------------------------------------------------------------------------
# Combined bundle: all three plugins in one zip.
# ---------------------------------------------------------------------------
COMBINED_ZIP="$RELEASE_DIR/superduper-dsp-${VERSION}-${PLATFORM}.zip"
(cd "$BUNDLE_TMP" && zip -qry "$COMBINED_ZIP" \
    SuperDuperReverb.clap SuperDuperSupermass.clap SuperDuperSpectrum.clap \
    SuperDuperSaturator.clap SuperDuperDelay.clap SuperDuperCompressor.clap \
    SuperDuperEq.clap SuperDuperLimiter.clap SuperDuperAmbient.clap \
    SuperDuperPad.clap SuperDuperVocal.clap)
echo "==> Packaged $COMBINED_ZIP"

# ---------------------------------------------------------------------------
# Checksums + install help.
# ---------------------------------------------------------------------------
(cd "$RELEASE_DIR" && shasum -a 256 *.zip > SHA256SUMS)
echo "==> Wrote $RELEASE_DIR/SHA256SUMS"

# Drop a README into the release dir with quick install steps so users who
# unpack the zip outside of GitHub still know what to do.
cp "$ROOT/INSTALL.md" "$RELEASE_DIR/INSTALL.md" 2>/dev/null || true

echo
echo "Release $VERSION ready in: $RELEASE_DIR"
ls -1 "$RELEASE_DIR"
