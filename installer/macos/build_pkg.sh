#!/bin/bash
# Build a macOS installer .pkg with a format chooser (CLAP / VST3 / AU).
#
# Usage:  installer/macos/build_pkg.sh <version>
# Inputs (already built by the release workflow):
#   dist/macos/*.clap        — CLAP bundles
#   dist/vst3/*.vst3          — VST3 wrapper bundles
#   dist/au/*.component       — AUv2 wrapper bundles
# Output:
#   dist/SuperDuperDSP-<version>-macos-arm64.pkg
#
# The VST3/AU wrappers load the matching .clap at runtime, so the CLAP
# component is marked required in the chooser and pre-selected.

set -euo pipefail
V="${1:?usage: build_pkg.sh <version>}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
PKGS="$WORK/pkgs"; mkdir -p "$PKGS"

# --- postinstall: strip quarantine so an un-notarised bundle still loads ---
mk_scripts() {
    local dir="$1" glob="$2"
    mkdir -p "$dir"
    cat > "$dir/postinstall" <<EOF
#!/bin/bash
xattr -dr com.apple.quarantine "$glob" 2>/dev/null || true
exit 0
EOF
    chmod +x "$dir/postinstall"
}

# --- one component pkg per format ---------------------------------------
build_component() {
    local id="$1" srcdir="$2" srcglob="$3" dest="$4" out="$5" quarglob="$6"
    [[ -n "$(ls -d $srcdir/$srcglob 2>/dev/null || true)" ]] || { echo "skip $id (no files)"; return 1; }
    local rootd="$WORK/root-$id"
    mkdir -p "$rootd$dest"
    cp -R $srcdir/$srcglob "$rootd$dest/"
    local scriptd="$WORK/scripts-$id"
    mk_scripts "$scriptd" "$quarglob"
    pkgbuild --root "$rootd" --scripts "$scriptd" \
        --identifier "$id" --version "$V" \
        --install-location "/" "$PKGS/$out"
}

HAVE_CLAP=0; HAVE_VST3=0; HAVE_AU=0
build_component co.superduperai.pkg.clap dist/macos "*.clap" \
    "/Library/Audio/Plug-Ins/CLAP" clap.pkg \
    "/Library/Audio/Plug-Ins/CLAP" && HAVE_CLAP=1
build_component co.superduperai.pkg.vst3 dist/vst3 "*.vst3" \
    "/Library/Audio/Plug-Ins/VST3" vst3.pkg \
    "/Library/Audio/Plug-Ins/VST3" && HAVE_VST3=1
build_component co.superduperai.pkg.au dist/au "*.component" \
    "/Library/Audio/Plug-Ins/Components" au.pkg \
    "/Library/Audio/Plug-Ins/Components" && HAVE_AU=1

# --- distribution: chooser UI -------------------------------------------
DIST="$WORK/distribution.xml"
{
    echo '<?xml version="1.0" encoding="utf-8"?>'
    echo '<installer-gui-script minSpecVersion="2">'
    echo '  <title>SuperDuper DSP</title>'
    echo '  <organization>co.superduperai</organization>'
    echo '  <options customize="always" require-scripts="false" hostArchitectures="arm64,x86_64"/>'
    echo '  <choices-outline>'
    [[ $HAVE_CLAP == 1 ]] && echo '    <line choice="clap"/>'
    [[ $HAVE_VST3 == 1 ]] && echo '    <line choice="vst3"/>'
    [[ $HAVE_AU   == 1 ]] && echo '    <line choice="au"/>'
    echo '  </choices-outline>'
    # CLAP required (wrappers depend on it); pre-selected + not unselectable.
    [[ $HAVE_CLAP == 1 ]] && cat <<XML
  <choice id="clap" title="CLAP (REAPER, Bitwig)" enabled="false" selected="true">
    <pkg-ref id="co.superduperai.pkg.clap"/>
  </choice>
XML
    [[ $HAVE_VST3 == 1 ]] && cat <<XML
  <choice id="vst3" title="VST3 (Cubase, Ableton, FL, DaVinci)" start_selected="true">
    <pkg-ref id="co.superduperai.pkg.vst3"/>
  </choice>
XML
    [[ $HAVE_AU == 1 ]] && cat <<XML
  <choice id="au" title="Audio Unit (Logic Pro)" start_selected="true">
    <pkg-ref id="co.superduperai.pkg.au"/>
  </choice>
XML
    [[ $HAVE_CLAP == 1 ]] && echo "  <pkg-ref id=\"co.superduperai.pkg.clap\" version=\"$V\">clap.pkg</pkg-ref>"
    [[ $HAVE_VST3 == 1 ]] && echo "  <pkg-ref id=\"co.superduperai.pkg.vst3\" version=\"$V\">vst3.pkg</pkg-ref>"
    [[ $HAVE_AU   == 1 ]] && echo "  <pkg-ref id=\"co.superduperai.pkg.au\" version=\"$V\">au.pkg</pkg-ref>"
    echo '</installer-gui-script>'
} > "$DIST"

mkdir -p dist
productbuild --distribution "$DIST" --package-path "$PKGS" \
    "dist/SuperDuperDSP-${V}-macos-arm64.pkg"
echo "==> dist/SuperDuperDSP-${V}-macos-arm64.pkg"
